use std::sync::Arc;

use crate::{config::Config, storage};

pub struct DownloadManager {
    database: Arc<storage::Database>,
    file_storage: Arc<storage::File>,
}

impl DownloadManager {
    pub fn new(
        _config: Arc<Config>,
        database: Arc<storage::Database>,
        file_storage: Arc<storage::File>,
    ) -> Self {
        DownloadManager {
            database,
            file_storage,
        }
    }
}
