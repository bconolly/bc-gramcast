//! Media domain: a video the owner has uploaded.

/// Upload size cap for version 1. Our Reels are about 2 MB, so this is generous.
pub const MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoUpload {
    pub file_name: String,
    pub size_bytes: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum UploadError {
    NotMp4,
    Empty,
    TooLarge { size_bytes: u64 },
    BadFileName,
}

impl VideoUpload {
    pub fn new(file_name: &str, content_type: &str, size_bytes: u64) -> Result<Self, UploadError> {
        if file_name.contains(['/', '\\']) || file_name.contains("..") {
            return Err(UploadError::BadFileName);
        }
        if content_type != "video/mp4" || !file_name.to_ascii_lowercase().ends_with(".mp4") {
            return Err(UploadError::NotMp4);
        }
        if size_bytes == 0 {
            return Err(UploadError::Empty);
        }
        if size_bytes > MAX_UPLOAD_BYTES {
            return Err(UploadError::TooLarge { size_bytes });
        }
        Ok(Self {
            file_name: file_name.to_owned(),
            size_bytes,
        })
    }

    /// Storage key for R2. Never uses the raw file name, so it can't escape the prefix.
    pub fn storage_key(&self, id: &str) -> String {
        format!("videos/{id}.mp4")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_an_mp4_under_the_cap() {
        let v = VideoUpload::new("reel-1.mp4", "video/mp4", 2_000_000).unwrap();
        assert_eq!(v.size_bytes, 2_000_000);
    }

    #[test]
    fn rejects_other_content_types() {
        assert_eq!(
            VideoUpload::new("reel.mov", "video/quicktime", 10),
            Err(UploadError::NotMp4)
        );
    }

    #[test]
    fn rejects_mp4_type_with_wrong_extension() {
        assert_eq!(
            VideoUpload::new("reel.exe", "video/mp4", 10),
            Err(UploadError::NotMp4)
        );
    }

    #[test]
    fn rejects_empty_files() {
        assert_eq!(
            VideoUpload::new("reel.mp4", "video/mp4", 0),
            Err(UploadError::Empty)
        );
    }

    #[test]
    fn rejects_files_over_the_cap() {
        let size = MAX_UPLOAD_BYTES + 1;
        assert_eq!(
            VideoUpload::new("reel.mp4", "video/mp4", size),
            Err(UploadError::TooLarge { size_bytes: size })
        );
    }

    #[test]
    fn rejects_path_like_file_names() {
        assert_eq!(
            VideoUpload::new("../x.mp4", "video/mp4", 10),
            Err(UploadError::BadFileName)
        );
        assert_eq!(
            VideoUpload::new("a/b.mp4", "video/mp4", 10),
            Err(UploadError::BadFileName)
        );
    }

    #[test]
    fn storage_key_uses_the_id_not_the_file_name() {
        let v = VideoUpload::new("my reel.mp4", "video/mp4", 10).unwrap();
        assert_eq!(v.storage_key("01JD3XYZ"), "videos/01JD3XYZ.mp4");
    }
}
