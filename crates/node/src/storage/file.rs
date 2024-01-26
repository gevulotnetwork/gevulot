use eyre::Result;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

pub struct File {
    data_dir: PathBuf,
}

impl File {
    pub fn new(data_dir: &Path) -> Self {
        File {
            data_dir: PathBuf::new().join(data_dir),
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
}
