use std::path::{Path, PathBuf};

use crate::types;
use eyre::Result;
use tokio::io::AsyncWriteExt;

pub struct File {
    client: reqwest::Client,
    data_dir: String,
}

impl File {
    pub fn new(data_dir: &Path) -> Self {
        File {
            client: reqwest::Client::new(),
            data_dir: data_dir.as_os_str().to_str().expect("filename").to_string(),
        }
    }

    pub async fn download(&self, file: &types::File) -> Result<()> {
        let file_name = Path::new(&file.name).file_name().unwrap();
        let file_path = PathBuf::new()
            .join(&self.data_dir)
            .join(file.task_id.to_string())
            .join(file_name);

        std::fs::create_dir_all(file_path.parent().unwrap())?;

        let url = reqwest::Url::parse(&file.url)?;
        let mut resp = self.client.get(url.clone()).send().await?;

        if resp.status() == reqwest::StatusCode::OK {
            let fd = tokio::fs::File::create(&file_path).await?;
            let mut fd = tokio::io::BufWriter::new(fd);

            while let Some(chunk) = resp.chunk().await? {
                fd.write_all(&chunk).await?;
            }

            fd.flush().await?;
            tracing::info!(
                "downloaded file to {}",
                file_path.into_os_string().to_str().unwrap().to_string()
            );
        } else {
            tracing::error!(
                "failed to download file from {}: response status: {}",
                url,
                resp.status()
            );
        }

        Ok(())
    }

    pub async fn get(
        &self,
        task_id: &types::TaskId,
        path: &str,
    ) -> Result<tokio::io::BufReader<tokio::fs::File>> {
        let file_name = Path::new(path).file_name().unwrap();
        let path = PathBuf::new()
            .join(&self.data_dir)
            .join(task_id.to_string())
            .join(file_name);
        let fd = tokio::fs::File::open(path).await?;
        Ok(tokio::io::BufReader::new(fd))
    }
}
