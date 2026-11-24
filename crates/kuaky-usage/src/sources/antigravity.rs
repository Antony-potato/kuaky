//! Antigravity CLI: `~/.gemini/antigravity-cli/conversations/**/*.db`, una
//! base SQLite por conversación.
//!
//! Formato documentado a partir de splitrail (MIT). La tabla `gen_metadata`
//! (`idx, data`) guarda en `data` un mensaje protobuf por respuesta del
//! modelo: campo 1 → { 19: id del modelo, 4: { 5: entrada, 2: salida,
//! 3: razonamiento } }. La fecha es el primer sub-mensaje con forma
//! `{ 1: segundos, 2: nanos }` dentro del mismo `data`. No trae tokens de caché.

use super::{Interner, Source};
use crate::{Agent, UsageEvent};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

pub struct Antigravity {
    dir: Option<PathBuf>,
}

impl Antigravity {
    pub fn from_env() -> Self {
        Self { dir: dirs::home_dir().map(|h| h.join(".gemini").join("antigravity-cli").join("conversations")) }
    }

    pub fn with_dir(dir: PathBuf) -> Self {
        Self { dir: Some(dir) }
    }
}

impl Source for Antigravity {
    fn agent(&self) -> Agent {
        Agent::Antigravity
    }

    fn load(&self) -> anyhow::Result<Vec<UsageEvent>> {
        let Some(dir) = &self.dir else { return Ok(Vec::new()) };
        let mut out = Vec::new();
        let mut interner = Interner::default();
        for db in super::files_with_ext(dir, "db") {
            match read_db(&db, &mut interner) {
                Ok(mut events) => out.append(&mut events),
                Err(err) => tracing::warn!(db = %db.display(), %err, "no se pudo leer Antigravity"),
            }
        }
        Ok(out)
    }
}

fn read_db(path: &Path, interner: &mut Interner) -> anyhow::Result<Vec<UsageEvent>> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let session = interner.get(&path.file_stem().unwrap_or_default().to_string_lossy());
    let fallback_ts = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| jiff::Timestamp::try_from(t).ok())
        .unwrap_or_else(jiff::Timestamp::now);
    let mut stmt = conn.prepare("SELECT idx, data FROM gen_metadata ORDER BY idx")?;
    let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(1))?;
    let mut out = Vec::new();
    for data in rows.flatten() {
        let Some(meta) = proto::message(&data, 1) else { continue };
        let tokens = proto::message(meta, 4);
        let num = |f: u32| tokens.and_then(|t| proto::varint(t, f)).unwrap_or(0);
        let (input, output, reasoning) = (num(5), num(2), num(3));
        if input + output + reasoning == 0 {
            continue;
        }
        let model = proto::string(meta, 19).unwrap_or("gemini");
        let ts = proto::find_timestamp(&data).unwrap_or(fallback_ts);
        out.push(UsageEvent {
            agent: Agent::Antigravity,
            ts,
            model: interner.get(model),
            session: session.clone(),
            project: None,
            input,
            output: output + reasoning,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: 0,
        });
    }
    Ok(out)
}

/// Lector mínimo del formato binario de protobuf (sin esquema).
mod proto {
    pub enum Value<'a> {
        Varint(u64),
        Bytes(&'a [u8]),
        Fixed,
    }

    fn read_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
        let mut result = 0u64;
        for shift in (0..64).step_by(7) {
            let b = *buf.get(*pos)?;
            *pos += 1;
            result |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Some(result);
            }
        }
        None
    }

    /// Campos `(número, valor)` de un mensaje; `None` si no es protobuf válido.
    pub fn fields(buf: &[u8]) -> Option<Vec<(u32, Value<'_>)>> {
        let mut pos = 0;
        let mut out = Vec::new();
        while pos < buf.len() {
            let key = read_varint(buf, &mut pos)?;
            let (field, wire) = ((key >> 3) as u32, key & 7);
            if field == 0 {
                return None;
            }
            let value = match wire {
                0 => Value::Varint(read_varint(buf, &mut pos)?),
                1 => {
                    pos = pos.checked_add(8).filter(|&p| p <= buf.len())?;
                    Value::Fixed
                }
                2 => {
                    let len = read_varint(buf, &mut pos)? as usize;
                    let end = pos.checked_add(len).filter(|&e| e <= buf.len())?;
                    let bytes = &buf[pos..end];
                    pos = end;
                    Value::Bytes(bytes)
                }
                5 => {
                    pos = pos.checked_add(4).filter(|&p| p <= buf.len())?;
                    Value::Fixed
                }
                _ => return None,
            };
            out.push((field, value));
        }
        Some(out)
    }

    pub fn message(buf: &[u8], field: u32) -> Option<&[u8]> {
        fields(buf)?.into_iter().find_map(|(f, v)| match v {
            Value::Bytes(b) if f == field => Some(b),
            _ => None,
        })
    }

    pub fn varint(buf: &[u8], field: u32) -> Option<u64> {
        fields(buf)?.into_iter().find_map(|(f, v)| match v {
            Value::Varint(n) if f == field => Some(n),
            _ => None,
        })
    }

    pub fn string(buf: &[u8], field: u32) -> Option<&str> {
        std::str::from_utf8(message(buf, field)?).ok()
    }

    /// Primer sub-mensaje con forma de `google.protobuf.Timestamp` plausible
    /// (año 2000 a 2100).
    pub fn find_timestamp(buf: &[u8]) -> Option<jiff::Timestamp> {
        let list = fields(buf)?;
        let secs = list.iter().find_map(|(f, v)| match v {
            Value::Varint(n) if *f == 1 => Some(*n),
            _ => None,
        });
        if let Some(s) = secs.filter(|s| (946_684_800..4_102_444_800).contains(s)) {
            if list.iter().all(|(f, _)| *f == 1 || *f == 2) {
                return jiff::Timestamp::from_second(s as i64).ok();
            }
        }
        list.iter().find_map(|(_, v)| match v {
            Value::Bytes(b) => find_timestamp(b),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(mut n: u64, out: &mut Vec<u8>) {
        loop {
            let b = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }
    fn field_varint(f: u32, n: u64) -> Vec<u8> {
        let mut out = Vec::new();
        varint(((f as u64) << 3) | 0, &mut out);
        varint(n, &mut out);
        out
    }
    fn field_bytes(f: u32, b: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        varint(((f as u64) << 3) | 2, &mut out);
        varint(b.len() as u64, &mut out);
        out.extend_from_slice(b);
        out
    }

    #[test]
    fn reads_generation_metadata() {
        let tokens = [field_varint(5, 1200), field_varint(2, 80), field_varint(3, 20)].concat();
        let time = [field_varint(1, 1_795_000_000), field_varint(2, 5)].concat();
        let meta = [field_bytes(19, b"gemini-4-argon"), field_bytes(4, &tokens), field_bytes(30, &time)].concat();
        let data = field_bytes(1, &meta);

        let dir = std::env::temp_dir().join(format!("kuaky-agy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = Connection::open(dir.join("conv-1.db")).unwrap();
        conn.execute_batch("CREATE TABLE gen_metadata (idx INTEGER, data BLOB);").unwrap();
        conn.execute("INSERT INTO gen_metadata VALUES (0, ?1)", [&data]).unwrap();
        conn.execute("INSERT INTO gen_metadata VALUES (1, ?1)", [&b"\xff\xff"[..]]).unwrap();
        drop(conn);

        let events = Antigravity::with_dir(dir.clone()).load().unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(events.len(), 1);
        assert_eq!(&*events[0].model, "gemini-4-argon");
        assert_eq!(events[0].input, 1200);
        assert_eq!(events[0].output, 100);
        assert_eq!(events[0].ts.as_second(), 1_795_000_000);
        assert_eq!(&*events[0].session, "conv-1");
    }
}
