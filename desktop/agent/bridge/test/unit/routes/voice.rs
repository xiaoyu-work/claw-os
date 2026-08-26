    use std::{
        io::Write as _,
        os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, symlink},
    };

    use super::*;

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new() -> Self {
            let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
            fs::create_dir_all(&target).expect("create test target directory");
            for _ in 0..TEMP_FILE_ATTEMPTS {
                let path = target.join(format!(
                    "voice-test-{}",
                    random_token().expect("generate test directory token")
                ));
                match fs::DirBuilder::new().mode(0o700).create(&path) {
                    Ok(()) => return Self { path },
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create private test directory: {error}"),
                }
            }
            panic!("allocate unique test directory");
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn selects_supported_mime_types_and_safe_extensions() {
        assert_eq!(
            select_audio_format("audio/wav"),
            Some(AudioFormat {
                mime_type: "audio/wav".into(),
                extension: "wav",
            })
        );
        assert_eq!(
            select_audio_format("Audio/WebM; codecs=opus"),
            Some(AudioFormat {
                mime_type: "audio/webm".into(),
                extension: "webm",
            })
        );
        assert_eq!(select_audio_format("audio/mpeg").unwrap().extension, "mp3");
        assert_eq!(select_audio_format("audio/mp4").unwrap().extension, "m4a");
        assert_eq!(select_audio_format("audio/flac").unwrap().extension, "flac");
        assert_eq!(select_audio_format("audio/ogg").unwrap().extension, "ogg");
        assert_eq!(select_audio_format("application/octet-stream"), None);
    }

    #[test]
    fn validates_empty_and_oversized_uploads() {
        assert_eq!(
            validate_upload(0, Some("audio/wav")),
            Err(ValidationError::EmptyBody)
        );
        assert_eq!(
            validate_upload(super::super::VOICE_MAX_BYTES + 1, Some("audio/wav")),
            Err(ValidationError::PayloadTooLarge)
        );
        assert!(validate_upload(super::super::VOICE_MAX_BYTES, Some("audio/wav")).is_ok());
        assert_eq!(
            validate_upload(1, Some("text/plain")),
            Err(ValidationError::UnsupportedMediaType)
        );
    }

    #[test]
    fn parses_direct_and_wrapped_transcription_json() {
        assert_eq!(
            parse_transcript_json(br#"{"text":"direct"}"#),
            Ok("direct".into())
        );
        assert_eq!(
            parse_transcript_json(br#"{"result":{"text":"wrapped"}}"#),
            Ok("wrapped".into())
        );
        assert_eq!(
            parse_transcript_json(br#"{"result":{}}"#),
            Err(TranscriptParseError::MissingText)
        );
        assert_eq!(
            parse_transcript_json(b"not json"),
            Err(TranscriptParseError::InvalidJson)
        );
    }

    #[tokio::test]
    async fn explicit_cleanup_removes_private_audio_file() {
        let directory = TestDirectory::new();
        let temp = create_temp_audio(directory.path(), "wav", b"RIFF")
            .await
            .expect("create temporary audio");
        let path = temp.path().to_path_buf();
        let metadata = fs::metadata(&path).expect("inspect temporary audio");
        assert_eq!(metadata.mode() & 0o777, 0o600);

        temp.cleanup().expect("remove temporary audio");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn drop_cleanup_removes_private_audio_file() {
        let directory = TestDirectory::new();
        let path = {
            let temp = create_temp_audio(directory.path(), "wav", b"RIFF")
                .await
                .expect("create temporary audio");
            temp.path().to_path_buf()
        };
        assert!(!path.exists());
    }

    #[test]
    fn private_create_does_not_follow_existing_symlink() {
        let directory = TestDirectory::new();
        let target = directory.path().join("target");
        fs::write(&target, b"unchanged").expect("write symlink target");
        let link = directory.path().join("voice.wav");
        symlink(&target, &link).expect("create symlink");

        let error = open_private_new_file(&link).expect_err("symlink must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(target).expect("read target"), b"unchanged");
    }

    #[tokio::test]
    async fn invokes_canonical_cli_and_parses_stub_result() {
        let directory = TestDirectory::new();
        let audio = create_temp_audio(directory.path(), "wav", b"RIFF")
            .await
            .expect("create temporary audio");
        let stub = directory.path().join("cos-stub");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&stub)
            .expect("create cos stub");
        file.write_all(
            br#"#!/bin/sh
if [ "$1" != "model" ] || [ "$2" != "transcribe" ] || [ ! -f "$3" ] || [ "$4" != "--format" ] || [ "$5" != "json" ]; then
  printf '%s\n' '{"error":"unexpected arguments"}'
  exit 2
fi
printf '%s\n' '{"result":{"text":"stub transcript"}}'
"#,
        )
        .expect("write cos stub");
        drop(file);

        let text =
            transcribe_audio_with_bin(audio.path(), stub.as_os_str(), Duration::from_secs(5))
                .await
                .expect("run transcription stub");
        assert_eq!(text, "stub transcript");
        audio.cleanup().expect("remove temporary audio");
    }
