//! SQLite backup, restore and integrity maintenance support.

use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx_core::{connection::Connection, query::query, row::Row};
use sqlx_sqlite::{SqliteConnectOptions, SqliteConnection};

use crate::config::Settings;

use super::database_path;

const BACKUP_FORMAT_VERSION: u32 = 2;
const SERVER_LOCK_FILE_NAME: &str = ".unionc-server.lock";
const MAINTENANCE_LOCK_FILE_NAME: &str = ".unionc-maintenance.lock";
static SERVER_DATABASE_LOCK: OnceLock<DatabaseFileLock> = OnceLock::new();

// Backup/restore is one private transactional boundary. Split its implementation
// by concern without exposing staging files or rollback guards outside it.
include!("lock.rs");

include!("backup.rs");

include!("restore.rs");

include!("validation.rs");

include!("manifest.rs");

include!("filesystem.rs");
