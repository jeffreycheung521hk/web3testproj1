use crate::domain::{Reason, ReconciliationStatus};
use crate::{
    Options,
    domain::{Payment, Report},
    error::{Result, invalid},
};
use fs2::FileExt;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use snafu::ResultExt;
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read},
    path::Path,
};
const LINE_LIMIT: u64 = 65536;
pub struct Input {
    reader: BufReader<File>,
    headers: csv::StringRecord,
    pub eof: bool,
}
impl Input {
    pub fn open(path: &Path) -> Result<Self> {
        let mut reader = BufReader::new(File::open(path)?);
        let header = line(&mut reader)?.ok_or_else(|| invalid("CSV is empty"))?;
        let headers = record(&header)?;
        let expected = [
            "payment_id",
            "tx_signature",
            "expected_sender_owner",
            "expected_recipient_owner",
            "expected_mint",
            "expected_amount_atomic",
        ];
        if headers.iter().collect::<Vec<_>>() != expected {
            return Err(invalid(
                "CSV headers must match the documented six columns in order",
            ));
        }
        Ok(Self {
            reader,
            headers,
            eof: false,
        })
    }
    pub fn read_payment(&mut self) -> Result<Option<Payment>> {
        match line(&mut self.reader)? {
            None => {
                self.eof = true;
                Ok(None)
            }
            Some(bytes) => {
                let row = record(&bytes)?;
                if row.len() != 6 {
                    return Err(invalid(
                        "CSV record must have six fields; embedded newlines unsupported",
                    ));
                }
                Ok(Some(row.deserialize(Some(&self.headers))?))
            }
        }
    }
}
fn line(reader: &mut BufReader<File>) -> Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    reader.take(LINE_LIMIT + 1).read_until(b'\n', &mut bytes)?;
    if bytes.len() as u64 > LINE_LIMIT {
        return Err(invalid("CSV physical line exceeds 64 KiB"));
    }
    if bytes.is_empty() {
        Ok(None)
    } else {
        Ok(Some(bytes))
    }
}
fn record(bytes: &[u8]) -> Result<csv::StringRecord> {
    // A quoted field may contain escaped quotes but never a physical newline.
    let mut quoted = false;
    for b in bytes {
        if *b == b'"' {
            quoted = !quoted;
        }
    }
    if quoted {
        return Err(invalid(
            "Unclosed quoted field / embedded newline unsupported",
        ));
    }
    let mut csv = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_reader(bytes);
    csv.records()
        .next()
        .ok_or_else(|| invalid("Blank CSV lines unsupported"))?
        .map_err(Into::into)
}
fn digest(path: &Path) -> Result<String> {
    let mut f = BufReader::new(File::open(path)?);
    let mut h = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}
pub struct Store {
    conn: Connection,
    pub progress: i64,
    _lock: File,
}
impl Store {
    pub fn open(o: &Options) -> Result<Self> {
        std::fs::create_dir_all(&o.output)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(o.output.join("run.lock"))?;
        lock.try_lock_exclusive().context(crate::error::IoSnafu {
            operation: "acquire output lock (directory may already be in use)",
        })?;
        let path = o.output.join("state.sqlite");
        let exists = path.exists();
        if exists != o.resume {
            return Err(invalid(if exists {
                "Output exists; use --resume or a new directory"
            } else {
                "Cannot resume: state database absent"
            }));
        }
        if !exists
            && std::fs::read_dir(&o.output)?
                .any(|entry| entry.map_or(true, |e| e.file_name() != "run.lock"))
        {
            return Err(invalid(
                "New output directory must be empty; existing files are preserved",
            ));
        }
        if !exists {
            std::fs::copy(&o.input, o.output.join("input.snapshot"))?;
            File::open(o.output.join("input.snapshot"))?.sync_all()?;
        }
        if digest(&o.input)? != digest(&o.output.join("input.snapshot"))? {
            return Err(invalid("Input differs from saved immutable snapshot"));
        }
        let fingerprint = DecisionFingerprint::new(o)?;
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA cache_size=-4096; CREATE TABLE IF NOT EXISTS config (fingerprint TEXT NOT NULL, progress INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS results (row INTEGER PRIMARY KEY, payment TEXT NOT NULL, signature TEXT NOT NULL, report TEXT NOT NULL); CREATE INDEX IF NOT EXISTS payment_idx ON results(payment); CREATE INDEX IF NOT EXISTS signature_idx ON results(signature);
            CREATE TABLE IF NOT EXISTS inputs (row INTEGER PRIMARY KEY, payment_json TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS revisions (id INTEGER PRIMARY KEY, row INTEGER NOT NULL, report TEXT NOT NULL, revised_at INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS revisions_row_idx ON revisions(row);
            CREATE INDEX IF NOT EXISTS review_idx ON results(json_extract(report,'$.reconciliation_status'),row);")?;
        let saved: Option<(String, i64)> = conn
            .query_row("SELECT fingerprint,progress FROM config", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        let progress = match saved {
            Some((saved, progress)) => {
                let saved: DecisionFingerprint = serde_json::from_str(&saved).map_err(|_| invalid("State uses previous decision/schema rules; rerun the same input in a NEW output directory, preserving the old directory for audit"))?;
                if saved.semantics != fingerprint.semantics
                    || saved.input_sha256 != fingerprint.input_sha256
                    || saved.commitment != fingerprint.commitment
                {
                    return Err(invalid(
                        "Resume rejected: input or decision settings changed; use a new output directory",
                    ));
                }
                progress
            }
            None => {
                let count: i64 =
                    conn.query_row("SELECT count(*) FROM results", [], |r| r.get(0))?;
                if count != 0 {
                    return Err(invalid("State is inconsistent: results without checkpoint"));
                }
                // A crash during first-time database creation committed no work. Initialize safely.
                conn.execute(
                    "INSERT INTO config VALUES (?1,0)",
                    [serde_json::to_string(&fingerprint)?],
                )?;
                0
            }
        };
        Ok(Self {
            conn,
            progress,
            _lock: lock,
        })
    }
    pub fn bind_network(&mut self, genesis: &str) -> Result<()> {
        let saved: String = self
            .conn
            .query_row("SELECT fingerprint FROM config", [], |row| row.get(0))?;
        let mut fingerprint: DecisionFingerprint = serde_json::from_str(&saved)?;
        match fingerprint.network_genesis_hash.as_deref() {
            Some(saved) if saved != genesis => {
                return Err(invalid(
                    "Resume rejected: RPC endpoint belongs to a different Solana network",
                ));
            }
            Some(_) => return Ok(()),
            None => {
                let count: i64 =
                    self.conn
                        .query_row("SELECT count(*) FROM results", [], |row| row.get(0))?;
                if count != 0 {
                    return Err(invalid("State contains results without a network identity"));
                }
                fingerprint.network_genesis_hash = Some(genesis.to_owned());
            }
        }
        self.conn.execute(
            "UPDATE config SET fingerprint=?1",
            [serde_json::to_string(&fingerprint)?],
        )?;
        Ok(())
    }

    pub fn commit(
        &mut self,
        reports: &mut [Report],
        payments: &[(i64, Payment)],
        progress: i64,
    ) -> Result<()> {
        self.save_batch(reports, payments, Some(progress))
    }

    pub fn revise(&mut self, reports: &mut [Report]) -> Result<()> {
        self.save_batch(reports, &[], None)
    }

    fn save_batch(
        &mut self,
        reports: &mut [Report],
        payments: &[(i64, Payment)],
        progress: Option<i64>,
    ) -> Result<()> {
        let transaction = self.conn.transaction()?;
        for report in reports {
            let prior: Option<(i64, String)> = transaction.query_row(
                "SELECT row,signature FROM results WHERE row<?3 AND (signature=?1 OR payment=?2) ORDER BY row LIMIT 1",
                params![report.signature, report.payment_id, report.input_row],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            if let Some((row, signature)) =
                prior.filter(|_| report.reconciliation_status != ReconciliationStatus::InvalidInput)
            {
                let previous_reason = report.reason;
                let previous_details = report.reason_details.clone();
                report.classify(
                    ReconciliationStatus::NeedsReview,
                    if signature == report.signature {
                        Reason::SignatureReuse
                    } else {
                        Reason::PaymentIdReuse
                    },
                );
                report.reason_details = format!(
                    "input_row={row};not_proven_duplicate_payment;original_reason={previous_reason};{previous_details}"
                );
            }
            if progress.is_none() {
                transaction.execute("INSERT INTO revisions(row,report,revised_at) SELECT row,report,?2 FROM results WHERE row=?1", params![report.input_row, i64::try_from(report.checked_at_unix).map_err(|_| invalid("Timestamp exceeds SQLite range"))?])?;
                let changed = transaction.execute(
                    "UPDATE results SET report=?2 WHERE row=?1",
                    params![report.input_row, serde_json::to_string(report)?],
                )?;
                if changed != 1 {
                    return Err(invalid("Review row disappeared"));
                }
            } else {
                transaction.execute(
                    "INSERT INTO results VALUES (?1,?2,?3,?4)",
                    params![
                        report.input_row,
                        report.payment_id,
                        report.signature,
                        serde_json::to_string(report)?
                    ],
                )?;
            }
        }
        for (row, payment) in payments {
            transaction.execute(
                "INSERT INTO inputs VALUES (?1,?2)",
                params![row, serde_json::to_string(payment)?],
            )?;
        }
        if let Some(progress) = progress {
            transaction.execute("UPDATE config SET progress=?1", [progress])?;
        }
        transaction.commit()?;
        if let Some(progress) = progress {
            self.progress = progress;
        }
        Ok(())
    }

    pub fn review_batch(
        &self,
        cursor: i64,
        upper: i64,
        limit: usize,
    ) -> Result<Vec<(i64, Payment)>> {
        let mut query = self.conn.prepare("SELECT results.row,inputs.payment_json FROM results JOIN inputs USING(row) WHERE results.row>?1 AND results.row<=?2 AND json_extract(results.report,'$.reconciliation_status')='needs_review' ORDER BY results.row LIMIT ?3")?;
        let mut rows = query.query(params![
            cursor,
            upper,
            i64::try_from(limit).map_err(|_| invalid("Batch exceeds SQLite range"))?
        ])?;
        let mut payments = Vec::with_capacity(limit);
        while let Some(row) = rows.next()? {
            payments.push((
                row.get(0)?,
                serde_json::from_str(&row.get::<_, String>(1)?)?,
            ));
        }
        Ok(payments)
    }

    pub fn export(&self, out: &Path) -> Result<()> {
        let temporary = out.join("report.csv.tmp");
        let mut writer = csv::WriterBuilder::new()
            .has_headers(false)
            .from_path(&temporary)?;
        writer.write_record([
            "input_row",
            "payment_id",
            "signature",
            "chain_status",
            "reconciliation_status",
            "reason",
            "reason_details",
            "slot",
            "block_time",
            "checked_at_unix",
            "fee_lamports",
            "evidence",
        ])?;
        let mut query = self
            .conn
            .prepare("SELECT report FROM results ORDER BY row")?;
        let mut rows = query.query([])?;
        while let Some(row) = rows.next()? {
            let r: Report = serde_json::from_str(&row.get::<_, String>(0)?)?;
            writer.serialize(r)?;
        }
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        std::fs::rename(temporary, out.join("report.csv"))?;
        File::open(out)
            .context(crate::error::IoSnafu {
                operation: "open output directory for report sync",
            })?
            .sync_all()
            .context(crate::error::IoSnafu {
                operation: "sync output directory after report rename",
            })?;
        Ok(())
    }
    pub fn write_run_manifest(
        &self,
        out: &Path,
        state: &str,
        diagnostic: Option<&str>,
        report_available: bool,
        processing_complete: bool,
    ) -> Result<()> {
        let rows: i64 = self
            .conn
            .query_row("SELECT count(*) FROM results", [], |row| row.get(0))?;
        let value = serde_json::json!({"state":state,"committed_rows":rows,"progress":self.progress,"diagnostic":diagnostic,"report":"report.csv","report_available":report_available,"processing_complete":processing_complete});
        let temporary = out.join("run-summary.json.tmp");
        let mut file = File::create(&temporary).context(crate::error::IoSnafu {
            operation: "create run summary",
        })?;
        serde_json::to_writer_pretty(&mut file, &value)?;
        file.sync_all().context(crate::error::IoSnafu {
            operation: "sync run summary",
        })?;
        std::fs::rename(temporary, out.join("run-summary.json"))?;
        File::open(out)?.sync_all()?;
        Ok(())
    }

    pub fn summary(&self) -> Result<()> {
        let mut q=self.conn.prepare("SELECT json_extract(report,'$.reconciliation_status'),count(*) FROM results GROUP BY 1")?;
        let mut rows = q.query([])?;
        while let Some(row) = rows.next()? {
            println!("{}: {}", row.get::<_, String>(0)?, row.get::<_, i64>(1)?);
        }
        Ok(())
    }
}
use rusqlite::OptionalExtension;
const DECISION_SEMANTICS: u32 = 3;
#[derive(Debug, Serialize, Deserialize)]
struct DecisionFingerprint {
    semantics: u32,
    input_sha256: String,
    commitment: String,
    network_genesis_hash: Option<String>,
}
impl DecisionFingerprint {
    fn new(options: &Options) -> Result<Self> {
        Ok(Self {
            semantics: DECISION_SEMANTICS,
            input_sha256: digest(&options.output.join("input.snapshot"))?,
            commitment: "finalized".to_owned(),
            network_genesis_hash: None,
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
