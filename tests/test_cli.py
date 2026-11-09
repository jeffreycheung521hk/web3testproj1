"""Acceptance tests drive the real binary against a local synthetic HTTP RPC."""
import csv
import io
import json
import os
from pathlib import Path
import signal
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'examples'))
from mock_rpc import Mock

BIN = os.environ['RECONCILE_BIN']
ROOT = Path(__file__).resolve().parents[1]
ROWS = list(csv.DictReader((ROOT / 'examples/payments.csv').open()))


class Acceptance(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.input = self.root / 'input.csv'
        self.output = self.root / 'output'
        self.write(ROWS)
        self.server = Mock()
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.url = f'http://127.0.0.1:{self.server.server_port}/credential-DO-NOT-LEAK'

    def write(self, rows):
        with self.input.open('w') as f:
            w = csv.DictWriter(f, fieldnames=ROWS[0]); w.writeheader(); w.writerows(rows)

    def command(self, *args):
        options = {'--input':str(self.input), '--output':str(self.output), '--rpc-url':self.url, '--batch-size':'4', '--max-concurrency':'2', '--max-retries':'1'}
        flags=[]; i=0
        while i<len(args):
            if args[i]=='--resume': flags.append(args[i]); i+=1
            else: options[args[i]]=args[i+1]; i+=2
        return [BIN,'reconcile',*[a for pair in options.items() for a in pair],*flags]

    def run_cli(self, *args, ok=True):
        result = subprocess.run(self.command(*args), capture_output=True, text=True, timeout=40)
        self.assertEqual(result.returncode == 0, ok, result.stderr)
        self.assertNotIn('DO-NOT-LEAK', result.stdout + result.stderr)
        self.assertNotIn('secret-from-provider', result.stdout + result.stderr)
        return result

    def report(self):
        with (self.output / 'report.csv').open() as f:
            return list(csv.DictReader(f))

    def test_classification_batching_concurrency_and_duplicates(self):
        self.server.delay = .02
        self.run_cli()
        r = self.report()
        self.assertEqual(len(r), len(ROWS))
        expected = ['matched', 'mismatch', 'mismatch', 'mismatch', 'mismatch', 'needs_review', 'needs_review', 'needs_review', 'needs_review', 'unsupported', 'needs_review', 'unsupported', 'needs_review', 'unsupported', 'needs_review', 'needs_review', 'matched', 'needs_review', 'needs_review']
        self.assertEqual([x['reconciliation_status'] for x in r], expected)
        self.assertEqual(r[5]['chain_status'], 'confirmed')
        self.assertEqual(r[6]['chain_status'], 'failed')
        self.assertEqual(r[7]['chain_status'], 'unknown')
        self.assertEqual(r[0]['fee_lamports'], '5000')
        self.assertEqual(json.loads(r[0]['evidence'])['amount_atomic'], 100)
        self.assertIn('not_proven_duplicate_payment', r[-1]['reason'])
        calls = [req for _, req in self.server.calls]
        self.assertEqual([len(q['params'][0]) for q in calls if q['method']=='getSignatureStatuses'], [4,4,4,4,3])
        self.assertLessEqual(self.server.peak, 2)
        self.assertEqual(self.server.peak, 2)
        self.assertNotIn('DO-NOT-LEAK', (self.output/'report.csv').read_text())

    def test_resume_regenerates_report_without_requests(self):
        self.run_cli(); before=self.report(); count=len(self.server.calls)
        (self.output/'report.csv').write_text('partial export after crash')
        self.run_cli('--resume')
        self.assertEqual(before,self.report()); self.assertEqual(count,len(self.server.calls))

    def test_resume_refuses_changed_input_or_configuration(self):
        self.run_cli()
        self.run_cli('--resume','--timeout-ms','7',ok=False)
        self.write(ROWS[:-1]); self.run_cli('--resume',ok=False)

    def test_atomic_batch_rollback_and_resume(self):
        self.run_cli()
        before = self.report()
        db=sqlite3.connect(self.output/'state.sqlite')
        # Simulate a process dying after writing output rows but before checkpoint update.
        db.execute('BEGIN')
        db.execute('DELETE FROM results WHERE row>4')
        db.execute('UPDATE config SET progress=4')
        db.commit()
        db.execute('BEGIN')
        db.execute('INSERT INTO results SELECT 5,payment,signature,report FROM results WHERE row=1')
        # Close without committing: both row and progress must be replayed together.
        db.close()
        self.run_cli('--resume')
        after=self.report()
        for a,b in zip(before,after):
            a.pop('checked_at_unix'); b.pop('checked_at_unix')
        self.assertEqual(before,after)
        self.assertEqual(len(set(x['input_row'] for x in after)),len(ROWS))

    def test_cancellation_then_resume(self):
        self.server.hold_after=5
        p=subprocess.Popen(self.command(),stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        try:
            deadline=time.monotonic()+10
            while time.monotonic()<deadline:
                if (self.output/'report.csv').exists() and len(self.server.calls)>5: break
                time.sleep(.02)
            else: self.fail('first batch did not commit')
            p.send_signal(signal.SIGINT); p.communicate(timeout=5)
            self.assertEqual(p.returncode,130)
        finally:
            if p.poll() is None: p.kill(); p.communicate()
        self.server.hold_after=None; self.server.release.set()
        self.run_cli('--resume')
        self.assertEqual(len(self.report()),len(ROWS))

    def one_row_fault(self, fault, timeout=10000):
        self.write(ROWS[:1]); self.server.faults=[fault]
        self.run_cli('--timeout-ms',str(timeout))

    def test_429_respects_retry_after(self):
        self.one_row_fault(429)
        self.assertEqual(self.report()[0]['reconciliation_status'],'matched')
        self.assertGreaterEqual(self.server.calls[1][0]-self.server.calls[0][0],.95)
        self.assertEqual(len(self.server.calls),3)

    def test_temporary_503_retries(self):
        self.one_row_fault(503)
        self.assertEqual(self.report()[0]['reconciliation_status'],'matched')
        self.assertEqual(len(self.server.calls),3)

    def test_timeout_retries(self):
        self.one_row_fault('timeout',50)
        self.assertEqual(self.report()[0]['reconciliation_status'],'matched')
        self.assertEqual(len(self.server.calls),3)

    def test_disconnect_retries(self):
        self.one_row_fault('disconnect')
        self.assertEqual(self.report()[0]['reconciliation_status'],'matched')

    def test_permanent_errors_and_size_do_not_retry(self):
        for fault in [401,400,'malformed','permanent_rpc','large']:
            with self.subTest(fault=fault):
                self.output=self.root/str(fault)
                self.server.calls=[]; self.one_row_fault(fault)
                self.assertEqual(len(self.server.calls),1)
                self.assertEqual(self.report()[0]['reconciliation_status'],'needs_review')
                self.assertEqual(self.report()[0]['chain_status'],'unknown')

    def test_retry_exhaustion_is_not_chain_failure(self):
        self.write(ROWS[:1]); self.server.faults=[503,503]
        self.run_cli()
        self.assertEqual(len(self.server.calls),2)
        self.assertEqual(self.report()[0]['chain_status'],'unknown')
        self.assertEqual(self.report()[0]['reconciliation_status'],'needs_review')

    def test_strict_integer_validation(self):
        for i,amount in enumerate(['1.0','-1','+1','01','18446744073709551616','0','1e2',' 1']):
            with self.subTest(amount=amount):
                self.output=self.root/f'invalid-{i}'
                self.write([dict(ROWS[0],expected_amount_atomic=amount)])
                self.run_cli()
                self.assertEqual(self.report()[0]['reconciliation_status'],'invalid_input')
        self.assertEqual(self.server.calls,[])

    def test_streaming_late_invalid_record_commits_earlier_batch(self):
        self.write(ROWS[:8])
        with self.input.open('a') as f: f.write('x'*65537+'\n')
        self.run_cli(ok=False)
        self.assertEqual(len(self.report()),8)
        self.assertEqual([len(q['params'][0]) for _,q in self.server.calls if q['method']=='getSignatureStatuses'],[4,4])

    def test_thousand_rows_cross_batches(self):
        self.write([dict(ROWS[0],payment_id=f'synthetic-load-{i}') for i in range(1001)])
        self.run_cli('--batch-size','128')
        self.assertEqual(len(self.report()),1001)
        self.assertEqual(len([q for _,q in self.server.calls if q['method']=='getSignatureStatuses']),8)
        self.assertLessEqual(self.server.peak,2)

    def test_u64_max_amount_is_exact(self):
        maximum = str(2**64-1)
        self.write([dict(ROWS[0],expected_amount_atomic=maximum)])
        tx=self.server.fixtures[ROWS[0]['tx_signature']]['transaction']
        tx['transaction']['message']['instructions'][0]['parsed']['info']['amount']=maximum
        tx['meta']['preTokenBalances'][0]['uiTokenAmount']['amount']=maximum
        tx['meta']['postTokenBalances'][0]['uiTokenAmount']['amount']='0'
        tx['meta']['postTokenBalances'][1]['uiTokenAmount']['amount']=maximum
        self.run_cli()
        self.assertEqual(self.report()[0]['reconciliation_status'],'matched')
        self.assertEqual(json.loads(self.report()[0]['evidence'])['amount_atomic'],2**64-1)

    def test_missing_or_ambiguous_evidence_never_matches(self):
        for i,kind in enumerate(['program_account','duplicate_balance','authority','slot','program_balance']):
            with self.subTest(kind=kind):
                self.server.fixtures=json.loads((ROOT/'examples/fixtures.json').read_text())['transactions']
                self.output=self.root/f'evidence-{i}'
                self.write(ROWS[:1])
                data=self.server.fixtures[ROWS[0]['tx_signature']]; tx=data['transaction']
                if kind=='program_account': tx['transaction']['message']['accountKeys'].pop()
                if kind=='duplicate_balance': tx['meta']['preTokenBalances'].append(tx['meta']['preTokenBalances'][0])
                if kind=='authority': tx['transaction']['message']['accountKeys'][2]['signer']=False
                if kind=='slot': del data['status']['slot']
                if kind=='program_balance': tx['meta']['postTokenBalances'][1]['programId']='spoof'
                self.run_cli()
                self.assertNotEqual(self.report()[0]['reconciliation_status'],'matched')

    def test_empty_input_has_report_headers_and_no_requests(self):
        self.write([]); self.run_cli()
        self.assertEqual(self.report(),[])
        self.assertTrue((self.output/'report.csv').read_text().startswith('input_row,'))
        self.assertEqual(self.server.calls,[])

    def test_preexisting_output_files_preserved(self):
        self.output.mkdir(); marker=self.output/'input.snapshot'; marker.write_text('user content')
        self.run_cli(ok=False)
        self.assertEqual(marker.read_text(),'user content')

    def test_resume_after_interrupted_database_initialization(self):
        self.output.mkdir()
        (self.output/'input.snapshot').write_bytes(self.input.read_bytes())
        sqlite3.connect(self.output/'state.sqlite').close()
        self.run_cli('--resume')
        self.assertEqual(len(self.report()),len(ROWS))


if __name__=='__main__':
    unittest.main(verbosity=2)
