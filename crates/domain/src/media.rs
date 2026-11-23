//! Media domain: a video the owner has uploaded.

/// Upload size cap: the largest video one publishing run can send within the platform's
/// subrequest limit (see `youtube`). Our Reels are about 2 MB, so this is generous.
pub const MAX_UPLOAD_BYTES: u64 = crate::youtube::MAX_PUBLISH_BYTES;
pub const MAX_FILE_NAME_BYTES: usize = 255;

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
    BadId,
}

/// The one safe id shape, `[A-Za-z0-9_-]{1,64}`, shared by storage keys and the
/// queue's video key check so the two can never drift apart.
pub fn is_safe_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl VideoUpload {
    pub fn new(file_name: &str, content_type: &str, size_bytes: u64) -> Result<Self, UploadError> {
        if file_name.len() > MAX_FILE_NAME_BYTES
            || file_name.contains(['/', '\\'])
            || file_name == ".."
            || file_name.chars().any(char::is_control)
        {
            return Err(UploadError::BadFileName);
        }
        let essence = content_type.split(';').next().unwrap_or("").trim();
        if !essence.eq_ignore_ascii_case("video/mp4")
            || !file_name.to_ascii_lowercase().ends_with(".mp4")
        {
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
    pub fn storage_key(&self, id: &str) -> Result<String, UploadError> {
        if !is_safe_id(id) {
            return Err(UploadError::BadId);
        }
        Ok(format!("videos/{id}.mp4"))
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
    fn storage_key_rejects_unsafe_ids() {
        let v = VideoUpload::new("reel.mp4", "video/mp4", 10).unwrap();
        assert_eq!(v.storage_key("../secrets/x"), Err(UploadError::BadId));
        assert_eq!(v.storage_key(""), Err(UploadError::BadId));
    }

    #[test]
    fn safe_id_shape_is_one_to_64_url_safe_chars() {
        assert!(is_safe_id("a"));
        assert!(is_safe_id("A-z_9"));
        assert!(is_safe_id(&"a".repeat(64)));
        for bad in ["", "a b", "a/b", "a.b", "é", "a\n", &"a".repeat(65)] {
            assert!(!is_safe_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn accepts_content_type_with_parameters_and_any_case() {
        assert!(VideoUpload::new("reel.mp4", "Video/MP4; codecs=avc1", 10).is_ok());
    }

    #[test]
    fn allows_double_dots_inside_a_name() {
        assert!(VideoUpload::new("a..b.mp4", "video/mp4", 10).is_ok());
    }

    #[test]
    fn rejects_control_characters_and_long_names() {
        assert_eq!(
            VideoUpload::new("a\u{0}b.mp4", "video/mp4", 10),
            Err(UploadError::BadFileName)
        );
        let long = format!("{}.mp4", "a".repeat(300));
        assert_eq!(
            VideoUpload::new(&long, "video/mp4", 10),
            Err(UploadError::BadFileName)
        );
    }

    #[test]
    fn storage_key_uses_the_id_not_the_file_name() {
        let v = VideoUpload::new("my reel.mp4", "video/mp4", 10).unwrap();
        assert_eq!(v.storage_key("01JD3XYZ").unwrap(), "videos/01JD3XYZ.mp4");
    }
}
