use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Durably writes `text` to a new private file in `dir`:
/// temp file → fsync → rename to a unique name → fsync(dir).
pub fn export(dir: &Path, text: &str) -> io::Result<PathBuf> {
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let base = format!("note-{}-{}", crate::db::now_ms(), std::process::id());
    let tmp = dir.join(format!(".{base}.tmp"));
    let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    let mut publish = || -> io::Result<PathBuf> {
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        let mut n = 1;
        let mut fin = dir.join(format!("{base}.txt"));
        while fin.exists() {
            n += 1;
            fin = dir.join(format!("{base}-{n}.txt"));
        }
        std::fs::rename(&tmp, &fin)?;
        File::open(dir)?.sync_all()?;
        Ok(fin)
    };
    publish().inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp); // e.g. disk full: don't leave an empty temp file behind
    })
}

/// Finished recovery files (`note-*.txt`), sorted; a missing dir is empty.
pub fn list(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut files = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with("note-") && name.ends_with(".txt") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn export_is_private_and_listed() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("recovery");
        let p = export(&dir, "héllo").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "héllo");
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        std::fs::write(dir.join(".junk.tmp"), "x").unwrap();
        assert_eq!(list(&dir).unwrap(), vec![p]);
    }

    #[test]
    fn list_missing_dir_is_empty() {
        assert!(list(Path::new("/nonexistent/qn")).unwrap().is_empty());
    }
}
