mod loading;
mod paths;
mod permissions;

use std::path::PathBuf;

fn home() -> PathBuf {
    PathBuf::from("/home/alice")
}
