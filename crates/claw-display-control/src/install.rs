use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::Error;

pub fn protected_executable(path: &str) -> Result<(), Error> {
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err(Error::Protocol("display executable must be absolute"));
    }
    for parent in path.ancestors().skip(1) {
        let metadata = std::fs::symlink_metadata(parent)?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(Error::Protocol("unprotected display executable ancestor"));
        }
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.nlink() != 1
        || metadata.mode() & 0o111 == 0
    {
        return Err(Error::Protocol("unprotected display executable"));
    }
    Ok(())
}
