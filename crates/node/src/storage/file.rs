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

    pub async fn get_task_file(
        &self,
        task_id: &str,
        path: &str,
    ) -> Result<tokio::io::BufReader<tokio::fs::File>> {
        let mut path = Path::new(path);
        if path.is_absolute() {
            path = path.strip_prefix("/")?;
        }
        let path = PathBuf::new().join(&self.data_dir).join(task_id).join(path);
        let fd = tokio::fs::File::open(path).await?;
        Ok(tokio::io::BufReader::new(fd))
    }

    pub async fn move_task_file(
        &self,
        task_id_src: &str,
        task_id_dst: &str,
        path: &str,
    ) -> Result<()> {
        let mut path = Path::new(path);
        if path.is_absolute() {
            path = path.strip_prefix("/")?;
        }

        let src_file_path = PathBuf::new()
            .join(&self.data_dir)
            .join(task_id_src)
            .join(path);
        let dst_file_path = PathBuf::new()
            .join(&self.data_dir)
            .join(task_id_dst)
            .join(path);

        tracing::debug!(
            "moving file from {:#?} to {:#?}",
            src_file_path,
            dst_file_path
        );

        // Ensure any necessary subdirectories exists.
        if let Some(parent) = dst_file_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .expect("task file mkdir");
        }

        tokio::fs::rename(src_file_path, dst_file_path)
            .await
            .map_err(|e| e.into())
    }

    pub async fn save_task_file(&self, task_id: &str, path: &str, data: Vec<u8>) -> Result<()> {
        let mut path = Path::new(path);
        if path.is_absolute() {
            path = path.strip_prefix("/")?;
        }

        let file_path = PathBuf::new().join(&self.data_dir).join(task_id).join(path);

        tracing::debug!(
            "saving task {} file {:#?} to {:#?}",
            task_id,
            path,
            file_path
        );

        // Ensure any necessary subdirectories exists.
        if let Some(parent) = file_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .expect("task file mkdir");
        }

        let fd = tokio::fs::File::create(&file_path).await?;
        let mut fd = tokio::io::BufWriter::new(fd);

        fd.write_all(data.as_slice()).await?;
        fd.flush().await?;

        tracing::debug!("file {:#?} successfully written", file_path);

        Ok(())
    }

    pub async fn download(&self, file: &types::File) -> Result<()> {
        let file_name = Path::new(&file.name).file_name().unwrap();
        let file_path = PathBuf::new()
            .join(&self.data_dir)
            .join(file.tx.to_string())
            .join(file_name);

        // Ensure any necessary subdirectories exists.
        if let Some(parent) = file_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .expect("download file mkdir");
        }

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
}
