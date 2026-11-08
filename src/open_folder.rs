use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn unescape_mount_path(bytes: &[u8]) -> Result<PathBuf, String> {
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            if i + 3 >= bytes.len()
                || !bytes[i + 1..=i + 3]
                    .iter()
                    .all(|b| (b'0'..=b'7').contains(b))
            {
                return Err("Invalid escaped path in mount information.".into());
            }
            let value = (bytes[i + 1] - b'0') as u16 * 64
                + (bytes[i + 2] - b'0') as u16 * 8
                + (bytes[i + 3] - b'0') as u16;
            decoded.push(
                u8::try_from(value).map_err(|_| "Invalid escaped path in mount information.")?,
            );
            i += 4;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    Ok(PathBuf::from(OsString::from_vec(decoded)))
}

fn find_mount(mountinfo: &[u8], configured: &str) -> Result<PathBuf, String> {
    let mut mounts = Vec::new();
    for line in mountinfo
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let Some(separator) = line.windows(3).position(|window| window == b" - ") else {
            return Err("Invalid mount information.".into());
        };
        let before = &line[..separator];
        let after = &line[separator + 3..];
        let mut filesystem = after.split(|byte| *byte == b' ');
        let (Some(kind), Some(source)) = (filesystem.next(), filesystem.next()) else {
            return Err("Invalid mount information.".into());
        };
        if (kind == b"fuse" || kind.starts_with(b"fuse.")) && source == b"pCloud.fs" {
            let mountpoint = before
                .split(|byte| *byte == b' ')
                .nth(4)
                .ok_or("Invalid mount information.")?;
            mounts.push(unescape_mount_path(mountpoint)?);
        }
    }
    match mounts.as_slice() {
        [] => Err("No pCloud drive is mounted. Start pCloudCC and try again.".into()),
        [mount] => Ok(mount.clone()),
        _ => {
            let configured = Path::new(configured.trim());
            if configured.is_absolute() {
                if let Some(mount) = mounts.iter().find(|mount| mount.as_path() == configured) {
                    return Ok(mount.clone());
                }
            }
            Err("Multiple pCloud drives are mounted. Set the mount point in Settings to choose one.".into())
        }
    }
}

pub fn open(configured: &str) -> Result<(), String> {
    let mountinfo = std::fs::read("/proc/self/mountinfo")
        .map_err(|error| format!("Could not read mounted filesystems: {error}"))?;
    let path = find_mount(&mountinfo, configured)?;
    open_with(&path, Path::new("xdg-open"))
}

fn open_with(path: &Path, opener: &Path) -> Result<(), String> {
    let status = opener_command(path, opener)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|error| format!("Could not open {}: {error}", path.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "Could not open {}: xdg-open exited with {status}.",
            path.display()
        ))
    }
}

fn opener_command(path: &Path, opener: &Path) -> Command {
    let mut command = Command::new(opener);
    command.arg(path);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_real_mount_and_decodes_spaces() {
        let info = b"1 0 0:1 / /tmp/other rw - fuse other rw\n2 0 0:2 / /home/me/My\\040Drive rw - fuse pCloud.fs rw\n";
        assert_eq!(
            find_mount(info, "").unwrap(),
            PathBuf::from("/home/me/My Drive")
        );
        assert_eq!(
            find_mount(info, "/wrong").unwrap(),
            PathBuf::from("/home/me/My Drive")
        );
    }

    #[test]
    fn rejects_missing_or_ambiguous_mounts() {
        assert!(find_mount(b"1 0 0:1 / /tmp/x rw - fuse other rw\n", "").is_err());
        let info =
            b"1 0 0:1 / /a rw - fuse pCloud.fs rw\n2 0 0:2 / /b rw - fuse.pcloud pCloud.fs rw\n";
        assert!(find_mount(info, "").is_err());
        assert_eq!(find_mount(info, "/b").unwrap(), PathBuf::from("/b"));
        assert!(unescape_mount_path(b"/bad\\999").is_err());
    }

    #[test]
    fn opener_failures_are_reported() {
        assert!(open_with(Path::new("/a b"), Path::new("/bin/false"))
            .unwrap_err()
            .contains("/a b"));
        assert!(open_with(Path::new("/a b"), Path::new("/missing-opener")).is_err());
        assert!(open_with(Path::new("/a b"), Path::new("/bin/true")).is_ok());
    }

    #[test]
    fn opener_receives_path_as_one_literal_argument() {
        let path = Path::new("/home/me/a b;$(touch bad)");
        let command = opener_command(path, Path::new("xdg-open"));
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, [path.as_os_str()]);
    }
}
