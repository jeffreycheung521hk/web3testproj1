use crate::domain::ReconciliationStatus;
use crate::{
    Options,
    domain::{Payment, Report},
    error::{Result, invalid},
};
use fs2::FileExt;
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
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
    pub fn next(&mut self) -> Result<Option<Payment>> {
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
        lock.try_lock_exclusive()
            .map_err(|_| invalid("Output directory is already in use"))?;
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
        let fingerprint = json_fingerprint(o)?;
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA cache_size=-4096; CREATE TABLE IF NOT EXISTS config (fingerprint TEXT NOT NULL, progress INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS results (row INTEGER PRIMARY KEY, payment TEXT NOT NULL, signature TEXT NOT NULL, report TEXT NOT NULL); CREATE INDEX IF NOT EXISTS payment_idx ON results(payment); CREATE INDEX IF NOT EXISTS signature_idx ON results(signature);")?;
        let saved: Option<(String, i64)> = conn
            .query_row("SELECT fingerprint,progress FROM config", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        let progress = match saved {
            Some((saved, progress)) => {
                if saved != fingerprint {
                    return Err(invalid(
                        "Resume rejected: input or decision settings changed",
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
                conn.execute("INSERT INTO config VALUES (?1,0)", [fingerprint])?;
                0
            }
        };
        Ok(Self {
            conn,
            progress,
            _lock: lock,
        })
    }
    pub fn commit(&mut self, reports: &mut [Report], progress: i64) -> Result<()> {
        let transaction = self.conn.transaction()?;
        for r in reports {
            let prior: Option<(i64, String)> = transaction.query_row(
                "SELECT row,signature FROM results WHERE signature=?1 OR payment=?2 ORDER BY row LIMIT 1",
                params![r.signature, r.payment_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            if let Some((row, sig)) =
                prior.filter(|_| r.reconciliation_status != ReconciliationStatus::InvalidInput)
            {
                r.classify(
                    ReconciliationStatus::NeedsReview,
                    &format!(
                        "{}:input_row={row};not_proven_duplicate_payment;original_reason={}",
                        if sig == r.signature {
                            "signature_reuse"
                        } else {
                            "payment_id_reuse"
                        },
                        r.reason
                    ),
                );
            }
            transaction.execute(
                "INSERT INTO results VALUES (?1,?2,?3,?4)",
                params![
                    r.input_row,
                    r.payment_id,
                    r.signature,
                    serde_json::to_string(r)?
                ],
            )?;
        }
        transaction.execute("UPDATE config SET progress=?1", [progress])?;
        transaction.commit()?;
        self.progress = progress;
        Ok(())
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
            "slot",
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
fn json_fingerprint(o: &Options) -> Result<String> {
    let value = serde_json::json!({"semantics":1,"input_sha256":digest(&o.output.join("input.snapshot"))?,"endpoint_sha256":hex(&Sha256::digest(o.rpc_url.as_deref().unwrap_or_default().as_bytes())),"batch":o.batch_size,"concurrency":o.max_concurrency,"timeout":o.timeout_ms,"retries":o.max_retries,"commitment":"finalized"});
    Ok(value.to_string())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
