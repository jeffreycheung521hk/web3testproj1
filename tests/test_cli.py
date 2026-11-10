"""Acceptance tests drive the real binary against a local synthetic HTTP RPC."""
import csv
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
with (ROOT / 'examples/payments.csv').open() as source:
    ROWS = list(csv.DictReader(source))


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
        with self.input.open('w') as target:
            writer = csv.DictWriter(target, fieldnames=ROWS[0])
            writer.writeheader()
            writer.writerows(rows)

    def command(self, *args):
        options = {
            '--input': str(self.input), '--output': str(self.output), '--rpc-url': self.url,
            '--batch-size': '4', '--max-concurrency': '2', '--max-retries': '1', '--max-rps': '1000',
        }
        flags = []
        index = 0
        while index < len(args):
            if args[index] in {'--resume', '--export-only', '--retry-review'}:
                flags.append(args[index])
                index += 1
            else:
                options[args[index]] = args[index + 1]
                index += 2
        if '--export-only' in flags:
            options.pop('--rpc-url', None)
        return [BIN, 'reconcile', *[arg for pair in options.items() for arg in pair], *flags]

    def run_cli(self, *args, ok=True):
        environment = os.environ.copy()
        if '--export-only' in args:
            environment.pop('RPC_URL', None)
        result = subprocess.run(
            self.command(*args), capture_output=True, text=True, timeout=50, env=environment,
        )
        self.assertEqual(result.returncode == 0, ok, result.stderr)
        self.assertNotIn('DO-NOT-LEAK', result.stdout + result.stderr)
        self.assertNotIn('secret-from-provider', result.stdout + result.stderr)
        return result

    def report(self):
        with (self.output / 'report.csv').open() as source:
            return list(csv.DictReader(source))

    def state(self):
        with sqlite3.connect(self.output / 'state.sqlite') as database:
            return (
                database.execute('SELECT progress FROM config').fetchone()[0],
                database.execute('SELECT count(*) FROM results').fetchone()[0],
            )

    def wait_for_progress(self, progress, process):
        deadline = time.monotonic() + 12
        while time.monotonic() < deadline:
            if process.poll() is not None:
                self.fail(f'process exited before checkpoint {progress}: {process.communicate()}')
            try:
                if self.state()[0] >= progress:
                    return
            except sqlite3.Error:
                pass
            time.sleep(.02)
        self.fail(f'checkpoint {progress} did not commit')

    def stop_process(self, process):
        if process.poll() is None:
            process.kill()
            process.communicate()
        self.server.hold_after = None
        self.server.release.set()

    def test_classification_batching_concurrency_and_duplicates(self):
        self.server.delay = .02
        self.run_cli()
        reports = self.report()
        self.assertEqual(len(reports), len(ROWS))
        expected = [
            'matched', 'mismatch', 'mismatch', 'mismatch', 'mismatch', 'needs_review',
            'needs_review', 'needs_review', 'needs_review', 'unsupported', 'needs_review',
            'unsupported', 'needs_review', 'unsupported', 'needs_review', 'needs_review',
            'matched', 'needs_review', 'needs_review',
        ]
        self.assertEqual([row['reconciliation_status'] for row in reports], expected)
        self.assertEqual(reports[5]['chain_status'], 'confirmed')
        self.assertEqual(reports[6]['chain_status'], 'failed')
        self.assertEqual(reports[7]['chain_status'], 'unknown')
        self.assertEqual(reports[0]['fee_lamports'], '5000')
        self.assertEqual(json.loads(reports[0]['evidence'])['amount_atomic'], 100)
        self.assertEqual(reports[-2]['reason'], 'signature_reuse')
        self.assertEqual(reports[-1]['reason'], 'payment_id_reuse')
        self.assertIn('input_row=1', reports[-1]['reason_details'])
        calls = [request for _, request in self.server.calls]
        self.assertEqual(
            [len(request['params'][0]) for request in calls if request['method'] == 'getSignatureStatuses'],
            [4, 4, 4, 4, 3],
        )
        self.assertLessEqual(self.server.peak, 2)
        self.assertEqual(self.server.peak, 2)
        self.assertNotIn('DO-NOT-LEAK', (self.output / 'report.csv').read_text())

    def test_resume_regenerates_report_without_payment_queries(self):
        self.run_cli()
        before = self.report()
        count = len(self.server.calls)
        (self.output / 'report.csv').write_text('partial export after crash')
        self.run_cli('--resume')
        self.assertEqual(before, self.report())
        self.assertEqual(count + 1, len(self.server.calls))

    def test_resume_accepts_operational_changes_but_refuses_changed_input(self):
        self.run_cli()
        count = len(self.server.calls)
        self.run_cli(
            '--resume', '--timeout-ms', '7', '--batch-size', '3', '--max-concurrency', '1',
            '--max-retries', '0', '--max-rps', '7', '--circuit-breaker-threshold', '1',
            '--rpc-url', self.url + '/new-provider-path',
        )
        self.assertEqual(len(self.server.calls), count + 1)
        self.write(ROWS[:-1])
        self.run_cli('--resume', ok=False)

    def test_resume_rejects_different_chain_identity(self):
        self.run_cli()
        before = self.report()
        self.server.genesis_hash = ROWS[0]['expected_mint']
        self.run_cli('--resume', '--rpc-url', self.url + '/other-chain', ok=False)
        self.assertEqual(before, self.report())
        self.assertEqual(self.state(), (len(ROWS), len(ROWS)))

    def test_final_export_failure_is_explicit_and_preserves_previous_csv(self):
        self.run_cli()
        before = self.report()
        (self.output / 'report.csv.tmp').mkdir()
        self.run_cli('--resume', ok=False)
        summary = json.loads((self.output / 'run-summary.json').read_text())
        self.assertEqual(summary['state'], 'export_error')
        self.assertFalse(summary['report_available'])
        self.assertTrue(summary['processing_complete'])
        self.assertEqual(self.report(), before)
        self.assertEqual(self.state(), (len(ROWS), len(ROWS)))
        (self.output / 'report.csv.tmp').rmdir()
        self.run_cli('--resume', '--export-only')
        self.assertEqual(self.report(), before)

    def test_missing_or_null_confirmation_status_remains_reviewable(self):
        for index, missing in enumerate([False, True]):
            with self.subTest(missing=missing):
                self.output = self.root / f'confirmation-{index}'
                self.write(ROWS[:1])
                status = self.server.fixtures[ROWS[0]['tx_signature']]['status']
                if missing:
                    status.pop('confirmationStatus', None)
                else:
                    status['confirmationStatus'] = None
                self.run_cli()
                self.assertEqual(self.report()[0]['reconciliation_status'], 'needs_review')
                self.assertEqual(self.report()[0]['chain_status'], 'unknown')

    def test_atomic_batch_rollback_and_resume(self):
        self.run_cli()
        before = self.report()
        database = sqlite3.connect(self.output / 'state.sqlite')
        database.execute('BEGIN')
        database.execute('DELETE FROM results WHERE row>4')
        database.execute('DELETE FROM inputs WHERE row>4')
        database.execute('UPDATE config SET progress=4')
        database.commit()
        database.execute('BEGIN')
        database.execute('INSERT INTO results SELECT 5,payment,signature,report FROM results WHERE row=1')
        # Simulate death after writing a row but before checkpoint/transaction commit.
        database.close()
        self.run_cli('--resume')
        after = self.report()
        for old, new in zip(before, after):
            old.pop('checked_at_unix')
            new.pop('checked_at_unix')
        self.assertEqual(before, after)
        self.assertEqual(len({row['input_row'] for row in after}), len(ROWS))

    def test_cancellation_then_resume(self):
        self.server.hold_after = 6
        process = subprocess.Popen(self.command(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.addCleanup(self.stop_process, process)
        self.wait_for_progress(4, process)
        self.assertFalse((self.output / 'report.csv').exists(), 'CSV must not be rewritten per batch')
        process.send_signal(signal.SIGINT)
        process.communicate(timeout=8)
        self.assertEqual(process.returncode, 130)
        self.assertEqual(self.state(), (4, 4))
        self.server.hold_after = None
        self.server.release.set()
        self.run_cli('--resume')
        self.assertEqual(len(self.report()), len(ROWS))

    def test_report_is_exported_only_after_processing_finishes(self):
        self.server.hold_after = 6
        process = subprocess.Popen(self.command(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.addCleanup(self.stop_process, process)
        self.wait_for_progress(4, process)
        self.assertFalse((self.output / 'report.csv').exists())
        self.server.hold_after = None
        self.server.release.set()
        stdout, stderr = process.communicate(timeout=12)
        self.assertEqual(process.returncode, 0, stdout + stderr)
        self.assertEqual(len(self.report()), len(ROWS))

    def test_export_only_recovers_after_hard_kill_without_rpc(self):
        self.server.hold_after = 6
        process = subprocess.Popen(self.command(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.addCleanup(self.stop_process, process)
        self.wait_for_progress(4, process)
        process.kill()
        process.communicate(timeout=5)
        self.server.hold_after = None
        self.server.release.set()
        count = len(self.server.calls)
        self.run_cli('--resume', '--export-only')
        self.assertEqual(len(self.report()), 4)
        self.assertEqual(len(self.server.calls), count)
        self.run_cli('--resume')
        self.assertEqual(len(self.report()), len(ROWS))

    def one_row_fault(self, fault, timeout=10000, ok=True):
        self.write(ROWS[:1])
        if fault == 'status_shape':
            self.server.method_faults['getSignatureStatuses'] = [fault]
        else:
            self.server.faults = [fault]
        return self.run_cli('--timeout-ms', str(timeout), ok=ok)

    def test_429_respects_retry_after(self):
        self.one_row_fault(429)
        self.assertEqual(self.report()[0]['reconciliation_status'], 'matched')
        self.assertGreaterEqual(self.server.calls[1][0] - self.server.calls[0][0], .95)
        self.assertEqual(len(self.server.calls), 4)

    def test_temporary_503_retries(self):
        self.one_row_fault(503)
        self.assertEqual(self.report()[0]['reconciliation_status'], 'matched')
        self.assertEqual(len(self.server.calls), 4)

    def test_timeout_retries(self):
        self.one_row_fault('timeout', 50)
        self.assertEqual(self.report()[0]['reconciliation_status'], 'matched')
        self.assertEqual(len(self.server.calls), 4)

    def test_disconnect_retries(self):
        self.one_row_fault('disconnect')
        self.assertEqual(self.report()[0]['reconciliation_status'], 'matched')

    def test_permanent_errors_shape_and_size_do_not_retry_or_commit_rows(self):
        for fault in [401, 400, 'malformed', 'permanent_rpc', 'large', 'status_shape']:
            with self.subTest(fault=fault):
                self.output = self.root / str(fault)
                self.server.calls = []
                self.one_row_fault(fault, ok=False)
                self.assertEqual(len(self.server.calls), 2 if fault == 'status_shape' else 1)
                self.assertEqual(self.state(), (0, 0))
                if (self.output / 'report.csv').exists():
                    self.assertEqual(self.report(), [])

    def test_retry_exhaustion_leaves_checkpoint_and_rows_untouched(self):
        self.write(ROWS[:1])
        self.server.faults = [503, 503]
        self.run_cli(ok=False)
        self.assertEqual(len(self.server.calls), 2)
        self.assertEqual(self.state(), (0, 0))
        self.server.faults = []
        self.run_cli('--resume', '--timeout-ms', '2000', '--rpc-url', self.url + '/replacement')
        self.assertEqual(self.report()[0]['reconciliation_status'], 'matched')
        self.assertEqual(self.state(), (1, 1))

    def test_transaction_rpc_error_rolls_back_entire_current_batch(self):
        self.write(ROWS[:4])
        self.server.signature_faults[ROWS[2]['tx_signature']] = 401
        self.run_cli(ok=False)
        self.assertEqual(self.state(), (0, 0))
        self.server.signature_faults = {}
        self.run_cli('--resume')
        self.assertEqual(len(self.report()), 4)
        self.assertEqual(self.state(), (4, 4))

    def test_transaction_identity_and_slot_disagreements_abort_current_batch(self):
        for index, kind in enumerate(['signature', 'slot', 'malformed_slot', 'missing_status_slot', 'unknown_confirmation', 'meta_execution_error', 'meta_invalid_error']):
            with self.subTest(kind=kind):
                self.server.fixtures = json.loads((ROOT / 'examples/fixtures.json').read_text())['transactions']
                self.output = self.root / f'identity-{index}'
                self.write(ROWS[:1])
                transaction = self.server.fixtures[ROWS[0]['tx_signature']]['transaction']
                if kind == 'signature':
                    transaction['transaction']['signatures'][0] = ROWS[1]['tx_signature']
                elif kind == 'slot':
                    transaction['slot'] += 1
                elif kind == 'malformed_slot':
                    transaction['slot'] = 'not-an-integer'
                elif kind == 'missing_status_slot':
                    del self.server.fixtures[ROWS[0]['tx_signature']]['status']['slot']
                elif kind == 'unknown_confirmation':
                    self.server.fixtures[ROWS[0]['tx_signature']]['status']['confirmationStatus'] = 'finalizd'
                elif kind == 'meta_execution_error':
                    transaction['meta']['err'] = {'InstructionError': [0, 'InvalidArgument']}
                else:
                    transaction['meta']['err'] = True
                self.run_cli(ok=False)
                self.assertEqual(self.state(), (0, 0))

    def test_infrastructure_failure_preserves_preceding_batches(self):
        self.server.fail_after = 6
        self.server.failure_after = 401
        self.run_cli(ok=False)
        self.assertEqual(self.state(), (4, 4))
        if (self.output / 'report.csv').exists():
            self.assertEqual(len(self.report()), 4)
        self.server.fail_after = None
        self.run_cli('--resume', '--batch-size', '3')
        self.assertEqual(len(self.report()), len(ROWS))
        self.assertEqual(len({row['input_row'] for row in self.report()}), len(ROWS))

    def test_circuit_breaker_stops_scheduling_more_http_requests(self):
        self.write(ROWS[:4])
        self.server.always_faults['getTransaction'] = 503
        self.server.retry_after = '0'
        self.run_cli('--max-retries', '0', '--circuit-breaker-threshold', '1', ok=False)
        self.assertLessEqual(len(self.server.calls), 4)
        self.assertEqual(self.state(), (0, 0))
        self.server.always_faults = {}
        self.run_cli('--resume')
        self.assertEqual(len(self.report()), 4)

    def test_global_rps_limit_applies_to_initial_and_retry_attempts(self):
        self.write(ROWS[:2])
        self.server.method_faults['getTransaction'] = [503]
        self.server.retry_after = '0'
        self.run_cli('--max-rps', '4', '--max-concurrency', '2')
        arrivals = [when for when, _ in self.server.calls]
        self.assertEqual(len(arrivals), 5)
        self.assertTrue(all(later - earlier >= .21 for earlier, later in zip(arrivals, arrivals[1:])), arrivals)
        self.assertEqual(len(self.report()), 2)

    def test_unsupported_version_rpc_is_a_reviewable_transaction_outcome(self):
        self.write(ROWS[:1])
        self.server.method_faults['getTransaction'] = ['unsupported_version']
        self.run_cli()
        self.assertEqual(self.report()[0]['reconciliation_status'], 'unsupported')
        self.assertEqual(self.report()[0]['chain_status'], 'finalized')

    def test_retry_review_updates_one_row_and_archives_previous_result(self):
        self.write(ROWS[5:6])
        self.run_cli()
        self.assertEqual(self.report()[0]['chain_status'], 'confirmed')
        self.server.fixtures[ROWS[5]['tx_signature']]['status']['confirmationStatus'] = 'finalized'
        self.run_cli('--resume', '--retry-review', '--batch-size', '1')
        reports = self.report()
        self.assertEqual(len(reports), 1)
        self.assertEqual(reports[0]['reconciliation_status'], 'matched')
        with sqlite3.connect(self.output / 'state.sqlite') as database:
            self.assertEqual(database.execute('SELECT count(*) FROM revisions').fetchone()[0], 1)

    def test_failed_retry_review_preserves_result_until_a_successful_recheck(self):
        self.write(ROWS[5:6])
        self.run_cli()
        before = self.report()
        self.server.fixtures[ROWS[5]['tx_signature']]['status']['confirmationStatus'] = 'finalized'
        self.server.signature_faults[ROWS[5]['tx_signature']] = 401
        self.run_cli('--resume', '--retry-review', ok=False)
        self.assertEqual(self.report(), before)
        self.assertEqual(self.state(), (1, 1))
        with sqlite3.connect(self.output / 'state.sqlite') as database:
            self.assertEqual(database.execute('SELECT count(*) FROM revisions').fetchone()[0], 0)
        self.server.signature_faults = {}
        self.run_cli('--resume', '--retry-review')
        self.assertEqual(self.report()[0]['reconciliation_status'], 'matched')
        with sqlite3.connect(self.output / 'state.sqlite') as database:
            self.assertEqual(database.execute('SELECT count(*) FROM revisions').fetchone()[0], 1)

    def test_compute_budget_and_memo_allow_direct_transfer_matching(self):
        self.write(ROWS[:1])
        transaction = self.server.fixtures[ROWS[0]['tx_signature']]['transaction']
        message = transaction['transaction']['message']
        compute = 'ComputeBudget111111111111111111111111111111'
        memo = 'MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr'
        message['accountKeys'].extend([
            {'pubkey': compute, 'signer': False}, {'pubkey': memo, 'signer': False},
        ])
        # Canonical base58 encoding for SetComputeUnitLimit(200000), tag 2 + little-endian u32.
        raw = bytes([2]) + (200000).to_bytes(4, 'little')
        number = int.from_bytes(raw, 'big')
        alphabet = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
        encoded = ''
        while number:
            number, remainder = divmod(number, 58)
            encoded = alphabet[remainder] + encoded
        transfer = message['instructions'][0]
        message['instructions'] = [
            {'programId': compute, 'accounts': [], 'data': encoded},
            transfer,
            {'programId': memo, 'parsed': 'synthetic invoice reference'},
        ]
        self.run_cli()
        self.assertEqual(self.report()[0]['reconciliation_status'], 'matched')

    def test_block_time_is_reported_separately_from_check_time(self):
        self.write(ROWS[:1])
        self.server.fixtures[ROWS[0]['tx_signature']]['transaction']['blockTime'] = 1700000000
        self.run_cli()
        report = self.report()[0]
        self.assertEqual(report['block_time'], '1700000000')
        self.assertGreater(int(report['checked_at_unix']), int(report['block_time']))

    def test_strict_integer_validation(self):
        for index, amount in enumerate(['1.0', '-1', '+1', '01', '18446744073709551616', '0', '1e2', ' 1']):
            with self.subTest(amount=amount):
                self.output = self.root / f'invalid-{index}'
                self.write([dict(ROWS[0], expected_amount_atomic=amount)])
                self.run_cli()
                self.assertEqual(self.report()[0]['reconciliation_status'], 'invalid_input')
        self.assertTrue(all(request['method'] == 'getGenesisHash' for _, request in self.server.calls))

    def test_streaming_late_invalid_record_commits_earlier_batch(self):
        self.write(ROWS[:8])
        with self.input.open('a') as target:
            target.write('x' * 65537 + '\n')
        self.run_cli(ok=False)
        self.assertEqual(self.state(), (8, 8))
        if (self.output / 'report.csv').exists():
            self.assertEqual(len(self.report()), 8)
        self.assertEqual(
            [len(request['params'][0]) for _, request in self.server.calls if request['method'] == 'getSignatureStatuses'],
            [4, 4],
        )

    def test_thousand_rows_cross_batches(self):
        self.write([dict(ROWS[0], payment_id=f'synthetic-load-{index}') for index in range(1001)])
        self.run_cli('--batch-size', '128')
        self.assertEqual(len(self.report()), 1001)
        self.assertEqual(len([request for _, request in self.server.calls if request['method'] == 'getSignatureStatuses']), 8)
        self.assertLessEqual(self.server.peak, 2)

    def test_u64_max_amount_is_exact(self):
        maximum = str(2**64 - 1)
        self.write([dict(ROWS[0], expected_amount_atomic=maximum)])
        transaction = self.server.fixtures[ROWS[0]['tx_signature']]['transaction']
        transaction['transaction']['message']['instructions'][0]['parsed']['info']['amount'] = maximum
        transaction['meta']['preTokenBalances'][0]['uiTokenAmount']['amount'] = maximum
        transaction['meta']['postTokenBalances'][0]['uiTokenAmount']['amount'] = '0'
        transaction['meta']['postTokenBalances'][1]['uiTokenAmount']['amount'] = maximum
        self.run_cli()
        self.assertEqual(self.report()[0]['reconciliation_status'], 'matched')
        self.assertEqual(json.loads(self.report()[0]['evidence'])['amount_atomic'], 2**64 - 1)

    def test_missing_or_ambiguous_evidence_never_matches(self):
        for index, kind in enumerate(['program_account', 'duplicate_balance', 'authority', 'program_balance']):
            with self.subTest(kind=kind):
                self.server.fixtures = json.loads((ROOT / 'examples/fixtures.json').read_text())['transactions']
                self.output = self.root / f'evidence-{index}'
                self.write(ROWS[:1])
                data = self.server.fixtures[ROWS[0]['tx_signature']]
                transaction = data['transaction']
                if kind == 'program_account':
                    transaction['transaction']['message']['accountKeys'].pop()
                if kind == 'duplicate_balance':
                    transaction['meta']['preTokenBalances'].append(transaction['meta']['preTokenBalances'][0])
                if kind == 'authority':
                    transaction['transaction']['message']['accountKeys'][2]['signer'] = False
                if kind == 'program_balance':
                    transaction['meta']['postTokenBalances'][1]['programId'] = 'spoof'
                self.run_cli()
                self.assertNotEqual(self.report()[0]['reconciliation_status'], 'matched')

    def test_empty_input_has_report_headers_and_no_payment_queries(self):
        self.write([])
        self.run_cli()
        self.assertEqual(self.report(), [])
        self.assertTrue((self.output / 'report.csv').read_text().startswith('input_row,'))
        self.assertTrue(all(request['method'] == 'getGenesisHash' for _, request in self.server.calls))

    def test_preexisting_output_files_preserved(self):
        self.output.mkdir()
        marker = self.output / 'input.snapshot'
        marker.write_text('user content')
        self.run_cli(ok=False)
        self.assertEqual(marker.read_text(), 'user content')

    def test_resume_after_interrupted_database_initialization(self):
        self.output.mkdir()
        (self.output / 'input.snapshot').write_bytes(self.input.read_bytes())
        sqlite3.connect(self.output / 'state.sqlite').close()
        self.run_cli('--resume')
        self.assertEqual(len(self.report()), len(ROWS))


if __name__ == '__main__':
    unittest.main(verbosity=2)
