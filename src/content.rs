use anyhow::{Context, Result, bail};
use async_zip::{Compression, ZipEntryBuilder, base::write::ZipFileWriter};
use bytes::Bytes;
use clap::ValueEnum;
use futures_util::{StreamExt, io::copy as futures_copy};
use std::{
    ffi::OsStr,
    fs, io,
    path::{Component, Path, PathBuf},
};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt, duplex},
    sync::mpsc,
};
use tokio_util::{compat::TokioAsyncReadCompatExt, io::ReaderStream};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ArchiveFormat {
    Zip,
}

#[derive(Clone, Debug)]
pub struct ContentSource {
    path: PathBuf,
    display_name: String,
    download_name: String,
    input_size: u64,
    kind: ContentKind,
    warnings: Vec<String>,
}

#[derive(Clone, Debug)]
enum ContentKind {
    File { size: u64 },
    DirectoryZip { entries: Vec<ArchiveEntry> },
}

#[derive(Clone, Debug)]
struct ArchiveEntry {
    archive_path: String,
    disk_path: Option<PathBuf>,
    is_dir: bool,
}

impl ContentSource {
    pub fn inspect(path: impl AsRef<Path>, archive_format: ArchiveFormat) -> Result<Self> {
        let path = path
            .as_ref()
            .canonicalize()
            .with_context(|| format!("unable to resolve {}", path.as_ref().display()))?;
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("unable to read metadata for {}", path.display()))?;

        if metadata.file_type().is_symlink() {
            bail!("Beam does not send symlinks directly in v1");
        }

        if metadata.is_file() {
            return Ok(Self {
                display_name: file_name_string(&path),
                download_name: file_name_string(&path),
                input_size: metadata.len(),
                kind: ContentKind::File {
                    size: metadata.len(),
                },
                path,
                warnings: Vec::new(),
            });
        }

        if metadata.is_dir() {
            match archive_format {
                ArchiveFormat::Zip => {
                    let (entries, warnings, input_size) = scan_directory_for_zip(&path)?;
                    let root_name = file_name_string(&path);
                    return Ok(Self {
                        display_name: root_name.clone(),
                        download_name: format!("{root_name}-beam.zip"),
                        input_size,
                        kind: ContentKind::DirectoryZip { entries },
                        path,
                        warnings,
                    });
                }
            }
        }

        bail!("Beam only supports regular files and directories in v1")
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn download_name(&self) -> &str {
        &self.download_name
    }

    pub fn input_size(&self) -> u64 {
        self.input_size
    }

    pub fn content_length(&self) -> Option<u64> {
        match self.kind {
            ContentKind::File { size } => Some(size),
            ContentKind::DirectoryZip { .. } => None,
        }
    }

    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            ContentKind::File { .. } => "file",
            ContentKind::DirectoryZip { .. } => "zip archive",
        }
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn supports_range(&self) -> bool {
        matches!(self.kind, ContentKind::File { .. })
    }

    pub async fn stream_to_channel(
        &self,
        tx: mpsc::Sender<Result<Bytes, io::Error>>,
        progress_tx: mpsc::UnboundedSender<u64>,
    ) -> io::Result<u64> {
        match &self.kind {
            ContentKind::File { .. } => self.stream_file(tx, progress_tx).await,
            ContentKind::DirectoryZip { entries } => {
                self.stream_directory_zip(entries.clone(), tx, progress_tx)
                    .await
            }
        }
    }

    pub async fn stream_range_to_channel(
        &self,
        start: u64,
        length: u64,
        tx: mpsc::Sender<Result<Bytes, io::Error>>,
        progress_tx: mpsc::UnboundedSender<u64>,
    ) -> io::Result<u64> {
        match self.kind {
            ContentKind::File { .. } => {
                self.stream_file_range(start, length, tx, progress_tx).await
            }
            ContentKind::DirectoryZip { .. } => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "range requests are not supported for directory archives",
            )),
        }
    }

    async fn stream_file(
        &self,
        tx: mpsc::Sender<Result<Bytes, io::Error>>,
        progress_tx: mpsc::UnboundedSender<u64>,
    ) -> io::Result<u64> {
        let file = File::open(&self.path).await?;
        let stream = ReaderStream::new(file);
        forward_stream(stream, tx, progress_tx).await
    }

    async fn stream_file_range(
        &self,
        start: u64,
        length: u64,
        tx: mpsc::Sender<Result<Bytes, io::Error>>,
        progress_tx: mpsc::UnboundedSender<u64>,
    ) -> io::Result<u64> {
        let mut file = File::open(&self.path).await?;
        file.seek(io::SeekFrom::Start(start)).await?;
        let stream = ReaderStream::new(file.take(length));
        forward_stream(stream, tx, progress_tx).await
    }

    async fn stream_directory_zip(
        &self,
        entries: Vec<ArchiveEntry>,
        tx: mpsc::Sender<Result<Bytes, io::Error>>,
        progress_tx: mpsc::UnboundedSender<u64>,
    ) -> io::Result<u64> {
        let (writer, reader) = duplex(64 * 1024);
        let zip_task = tokio::spawn(async move { write_directory_zip(entries, writer).await });
        let total_sent = forward_stream(ReaderStream::new(reader), tx, progress_tx).await;

        match zip_task.await {
            Ok(Ok(())) => total_sent,
            Ok(Err(error)) => {
                if total_sent.is_err() {
                    total_sent
                } else {
                    Err(error)
                }
            }
            Err(error) => Err(io::Error::new(io::ErrorKind::Other, error.to_string())),
        }
    }
}

async fn forward_stream<S>(
    mut stream: S,
    tx: mpsc::Sender<Result<Bytes, io::Error>>,
    progress_tx: mpsc::UnboundedSender<u64>,
) -> io::Result<u64>
where
    S: futures_util::Stream<Item = io::Result<Bytes>> + Unpin,
{
    let mut total = 0_u64;
    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chunk) => {
                total += chunk.len() as u64;
                if tx.send(Ok(chunk)).await.is_err() {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "client disconnected",
                    ));
                }
                let _ = progress_tx.send(total);
            }
            Err(error) => {
                let error_copy = io::Error::new(error.kind(), error.to_string());
                let _ = tx
                    .send(Err(io::Error::new(error.kind(), error.to_string())))
                    .await;
                return Err(error_copy);
            }
        }
    }

    Ok(total)
}

async fn write_directory_zip(
    entries: Vec<ArchiveEntry>,
    writer: tokio::io::DuplexStream,
) -> io::Result<()> {
    let mut zip = ZipFileWriter::with_tokio(writer);
    for entry in entries {
        let compression = if entry.is_dir {
            Compression::Stored
        } else {
            Compression::Deflate
        };
        let builder = ZipEntryBuilder::new(entry.archive_path.clone().into(), compression);

        if entry.is_dir {
            zip.write_entry_whole(builder, &[])
                .await
                .map_err(to_io_error)?;
            continue;
        }

        let Some(disk_path) = entry.disk_path else {
            continue;
        };

        let source = File::open(disk_path).await?;
        let mut source = source.compat();
        let mut entry_writer = zip.write_entry_stream(builder).await.map_err(to_io_error)?;
        let _: u64 = futures_copy(&mut source, &mut entry_writer)
            .await
            .map_err(to_io_error)?;
        entry_writer.close().await.map_err(to_io_error)?;
    }

    zip.close().await.map_err(to_io_error)?;
    Ok(())
}

fn scan_directory_for_zip(root: &Path) -> Result<(Vec<ArchiveEntry>, Vec<String>, u64)> {
    let root_name = file_name_string(root);
    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    let mut input_size = 0_u64;

    visit_directory(
        root,
        root,
        &root_name,
        &mut entries,
        &mut warnings,
        &mut input_size,
    )?;

    if entries.is_empty() {
        entries.push(ArchiveEntry {
            archive_path: format!("{root_name}/"),
            disk_path: None,
            is_dir: true,
        });
    }

    Ok((entries, warnings, input_size))
}

fn visit_directory(
    root: &Path,
    current: &Path,
    root_name: &str,
    entries: &mut Vec<ArchiveEntry>,
    warnings: &mut Vec<String>,
    input_size: &mut u64,
) -> Result<bool> {
    let mut dir_entries = fs::read_dir(current)
        .with_context(|| format!("unable to read {}", current.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("unable to inspect {}", current.display()))?;
    dir_entries.sort_by_key(|entry| entry.path());

    let mut contains_supported_content = false;

    for dir_entry in dir_entries {
        let path = dir_entry.path();
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("unable to read metadata for {}", path.display()))?;
        let relative = path.strip_prefix(root).unwrap_or(&path);
        let archive_path = to_archive_path(root_name, relative, metadata.is_dir());

        if metadata.file_type().is_symlink() {
            warnings.push(format!("Skipping symlink {}", relative.display()));
            continue;
        }

        if metadata.is_file() {
            *input_size += metadata.len();
            entries.push(ArchiveEntry {
                archive_path,
                disk_path: Some(path),
                is_dir: false,
            });
            contains_supported_content = true;
            continue;
        }

        if metadata.is_dir() {
            let child_has_content =
                visit_directory(root, &path, root_name, entries, warnings, input_size)?;
            if !child_has_content {
                entries.push(ArchiveEntry {
                    archive_path,
                    disk_path: None,
                    is_dir: true,
                });
            }
            contains_supported_content = true;
            continue;
        }

        warnings.push(format!("Skipping unsupported entry {}", relative.display()));
    }

    Ok(contains_supported_content)
}

fn to_archive_path(root_name: &str, relative: &Path, is_dir: bool) -> String {
    let mut parts = vec![root_name.to_string()];
    parts.extend(relative.components().filter_map(component_to_string));
    let joined = parts.join("/");

    if is_dir { format!("{joined}/") } else { joined }
}

fn component_to_string(component: Component<'_>) -> Option<String> {
    match component {
        Component::Normal(part) => Some(part.to_string_lossy().to_string()),
        _ => None,
    }
}

fn file_name_string(path: &Path) -> String {
    path.file_name()
        .unwrap_or_else(|| OsStr::new("beam"))
        .to_string_lossy()
        .to_string()
}

fn to_io_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::Other, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{ArchiveFormat, ContentSource};
    use std::{fs, os::unix::fs::symlink};
    use tempfile::tempdir;

    #[test]
    fn names_directory_archives() {
        let temp = tempdir().unwrap();
        let folder = temp.path().join("assets");
        fs::create_dir_all(folder.join("nested")).unwrap();
        fs::write(folder.join("nested").join("a.txt"), b"beam").unwrap();

        let source = ContentSource::inspect(&folder, ArchiveFormat::Zip).unwrap();
        assert_eq!(source.download_name(), "assets-beam.zip");
        assert_eq!(source.kind_label(), "zip archive");
    }

    #[test]
    fn warns_about_symlinks_in_directories() {
        let temp = tempdir().unwrap();
        let folder = temp.path().join("bundle");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("file.txt"), b"ok").unwrap();
        symlink(folder.join("file.txt"), folder.join("alias.txt")).unwrap();

        let source = ContentSource::inspect(&folder, ArchiveFormat::Zip).unwrap();
        assert_eq!(source.warnings().len(), 1);
    }
}
