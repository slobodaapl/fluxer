use super::*;
use std::io::{Cursor, Write};

fn clipboard(bytes: &[u8]) -> AttachmentInput {
    AttachmentInput::Clipboard {
        filename: "../generated.bin".into(),
        mime_type: "application/octet-stream".into(),
        bytes: Arc::new(bytes.to_vec()),
    }
}
fn prepared(bytes: &[u8]) -> Arc<PreparedAttachment> {
    attachments::prepare(clipboard(bytes), 1024 * 1024, 7, &UploadCancellation::new()).unwrap()
}
fn demo() -> (Worker, mpsc::Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    let mut worker = Worker::new(tx, Arc::new(Mutex::new(BTreeMap::new())));
    worker.demo = true;
    worker.state = ClientState::demo();
    worker.state.select_channel(11.into());
    (worker, rx)
}
async fn settle_preparation(worker: &mut Worker, channel: Snowflake) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while worker
            .attachments
            .channels
            .get(&channel)
            .into_iter()
            .flatten()
            .any(|stage| stage.job.is_some())
        {
            let event = worker.attachments.rx.recv().await.unwrap();
            worker.attachment_event(event);
        }
    })
    .await
    .unwrap();
}

#[test]
fn clipboard_binary_bytes_are_private_streamable_and_removed_with_the_last_owner() {
    let bytes = b"synthetic\0\xffattachment";
    let source = prepared(bytes);
    let path = source.path.clone();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(source.mime_type, "application/octet-stream");
    assert!(source.preview.is_none());
    assert_eq!(clipboard(bytes).filename(), "generated.bin");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let other = source.clone();
    drop(source);
    assert!(path.exists());
    drop(other);
    assert!(!path.exists());
}

#[test]
fn generated_image_preview_is_bounded_bgra_without_changing_uploaded_bytes() {
    let image = image::RgbaImage::from_pixel(640, 320, image::Rgba([10, 20, 30, 255]));
    let mut encoded = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    let bytes = encoded.into_inner();
    let source = prepared(&bytes);
    let preview = source.preview.as_ref().unwrap();
    assert_eq!((preview.width, preview.height), (256, 128));
    assert_eq!(preview.bgra.len(), 256 * 128 * 4);
    assert_eq!(&preview.bgra[..4], &[30, 20, 10, 255]);
    assert_eq!(source.mime_type, "image/png");
    assert_eq!(std::fs::read(&source.path).unwrap(), bytes);
    assert!(!format!("{preview:?}").contains("30, 20"));
}

#[test]
fn oversized_or_cancelled_sources_do_not_produce_a_prepared_attachment() {
    assert!(attachments::prepare(clipboard(b"12345"), 4, 1, &UploadCancellation::new()).is_err());
    let cancel = UploadCancellation::new();
    cancel.cancel();
    assert!(attachments::prepare(clipboard(b"synthetic"), 100, 1, &cancel).is_err());
    let directory = tempfile::tempdir().unwrap();
    assert!(
        attachments::prepare(
            AttachmentInput::Disk(directory.path().into()),
            100,
            1,
            &UploadCancellation::new()
        )
        .is_err()
    );
}

#[tokio::test]
async fn changed_disk_file_is_rejected_but_staged_clipboard_remains_valid() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"before").unwrap();
    let source = attachments::prepare(
        AttachmentInput::Disk(file.path().into()),
        100,
        1,
        &UploadCancellation::new(),
    )
    .unwrap();
    source.validate_source().await.unwrap();
    file.write_all(b"after").unwrap();
    assert!(
        source
            .validate_source()
            .await
            .unwrap_err()
            .contains("changed")
    );
    prepared(b"unchanged").validate_source().await.unwrap();
}

#[tokio::test]
async fn pending_preparation_reserves_count_and_reset_ignores_old_account_results() {
    let (mut worker, events) = demo();
    let channel = Snowflake(11);
    let permit = worker
        .attachments
        .preparation_slots
        .clone()
        .acquire_many_owned(4)
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("generated.bin");
    std::fs::write(&path, b"test").unwrap();
    worker.stage_files(channel, vec![path; 10]).unwrap();
    assert!(
        worker
            .stage_clipboard(
                channel,
                "extra.bin".into(),
                "application/octet-stream".into(),
                Arc::new(vec![1])
            )
            .is_err()
    );
    let old_epoch = worker.attachments.epoch;
    let old_id = worker.attachments.channels[&channel][0].dto.id;
    worker.clear_attachments();
    worker
        .stage_clipboard(
            channel,
            "next-account.bin".into(),
            "application/octet-stream".into(),
            Arc::new(vec![2]),
        )
        .unwrap();
    let new_id = worker.attachments.channels[&channel][0].dto.id;
    assert!(new_id > old_id);
    let old_result = prepared(b"old account bytes");
    let old_path = old_result.path.clone();
    worker.attachment_event(AttachmentEvent::Prepared {
        epoch: old_epoch,
        channel,
        id: old_id,
        revision: 1,
        result: Ok(old_result),
    });
    assert!(!old_path.exists());
    assert_eq!(worker.attachments.channels[&channel].len(), 1);
    assert_eq!(
        worker.attachments.channels[&channel][0].dto.filename,
        "next-account.bin"
    );
    drop(permit);
    settle_preparation(&mut worker, channel).await;
    assert_eq!(
        worker.attachments.channels[&channel][0].dto.status,
        AttachmentStatus::Staged
    );
    assert!(
        events
            .try_iter()
            .any(|event| matches!(event, Event::AttachmentsCleared))
    );
    assert!(worker.media.is_none());
}

#[tokio::test]
async fn cancelling_preparation_can_retry_without_accepting_the_cancelled_completion() {
    let (mut worker, _) = demo();
    let channel = Snowflake(11);
    let permit = worker
        .attachments
        .preparation_slots
        .clone()
        .acquire_many_owned(4)
        .await
        .unwrap();
    worker
        .stage_clipboard(
            channel,
            "bytes.bin".into(),
            "application/octet-stream".into(),
            Arc::new(vec![1, 2]),
        )
        .unwrap();
    let stage = &worker.attachments.channels[&channel][0];
    let id = stage.dto.id;
    let cancel = stage.job.as_ref().unwrap().cancel.clone().unwrap();
    worker.cancel_attachment(channel, id, false).unwrap();
    assert!(cancel.is_cancelled());
    assert_eq!(
        worker.attachments.channels[&channel][0].dto.status,
        AttachmentStatus::Cancelled
    );
    worker.retry_attachment(channel, id).unwrap();
    drop(permit);
    settle_preparation(&mut worker, channel).await;
    assert_eq!(
        worker.attachments.channels[&channel][0].dto.status,
        AttachmentStatus::Staged
    );
    assert_eq!(
        worker.attachments.channels[&channel][0]
            .prepared
            .as_ref()
            .unwrap()
            .size,
        2
    );
}

#[tokio::test]
async fn privacy_change_cancels_inflight_preupload_and_fences_its_result() {
    let (mut worker, _) = demo();
    let channel = Snowflake(11);
    worker
        .stage_clipboard(
            channel,
            "bytes.bin".into(),
            "application/octet-stream".into(),
            Arc::new(vec![1, 2]),
        )
        .unwrap();
    settle_preparation(&mut worker, channel).await;
    worker.demo = false;
    let mut instance =
        Instance::from_endpoints("http://127.0.0.1:1/api", "ws://127.0.0.1:1/gateway").unwrap();
    instance.features = json!({"presigned_attachment_uploads":true});
    worker.client = Some(Client::new(instance).unwrap());
    let epoch = worker.attachments.epoch;
    let stage = &mut worker.attachments.channels.get_mut(&channel).unwrap()[0];
    let (id, revision) = (stage.dto.id, stage.revision);
    let cancel = UploadCancellation::new();
    stage.job = Some(Job {
        task: tokio::spawn(pending()),
        cancel: Some(cancel.clone()),
    });
    stage.dto.status = AttachmentStatus::Uploading;
    stage.uploaded = Some(json!({"upload_filename":"private-cache"}));
    worker.state.user_settings = json!({"synced_preferences":"ggUCGAA="});
    worker.refresh_attachment_policy(channel);
    assert!(cancel.is_cancelled());
    worker.attachment_event(AttachmentEvent::Uploaded {
        epoch,
        channel,
        id,
        revision,
        result: Ok(json!({"upload_filename":"late-private-cache"})),
    });
    let stage = &worker.attachments.channels[&channel][0];
    assert!(stage.uploaded.is_none());
    assert!(stage.job.is_none());
    assert_eq!(stage.dto.status, AttachmentStatus::Staged);
}

#[tokio::test]
async fn cancelled_batch_member_preserves_sibling_plan_and_final_removal_aborts_it() {
    let (mut worker, _) = demo();
    let channel = Snowflake(11);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("generated.bin");
    std::fs::write(&path, b"test").unwrap();
    worker
        .stage_files(channel, vec![path.clone(), path])
        .unwrap();
    settle_preparation(&mut worker, channel).await;
    let stages = worker.attachments.channels.get_mut(&channel).unwrap();
    let (first, second, batch) = (stages[0].dto.id, stages[1].dto.id, stages[0].batch);
    assert_eq!(stages[1].batch, batch);
    let members = stages
        .iter_mut()
        .map(|stage| {
            stage.planning = Some(batch);
            (stage.dto.id, stage.revision)
        })
        .collect();
    let cancel = UploadCancellation::new();
    worker.attachments.plans.insert(
        batch,
        PreuploadPlan {
            members,
            _job: Job {
                task: tokio::spawn(pending()),
                cancel: Some(cancel.clone()),
            },
        },
    );
    worker.cancel_attachment(channel, first, true).unwrap();
    assert!(
        !cancel.is_cancelled(),
        "a sibling still owns the shared planning request"
    );
    worker.cancel_attachment(channel, second, true).unwrap();
    assert!(cancel.is_cancelled());
    assert!(worker.attachments.plans.is_empty());
}

#[tokio::test]
async fn cancelling_queued_send_stops_its_plan_and_keeps_new_files_and_draft() {
    let (mut worker, events) = demo();
    let channel = Snowflake(11);
    worker
        .stage_clipboard(
            channel,
            "first.bin".into(),
            "application/octet-stream".into(),
            Arc::new(vec![1]),
        )
        .unwrap();
    settle_preparation(&mut worker, channel).await;
    let stage = &mut worker.attachments.channels.get_mut(&channel).unwrap()[0];
    let id = stage.dto.id;
    // A preparation-independent HTTP job remains owned by its selected stage.
    let cancel = UploadCancellation::new();
    stage.job = Some(Job {
        task: tokio::spawn(pending()),
        cancel: Some(cancel.clone()),
    });
    worker.state.set_draft(channel, "original".into());
    // Demo normally has no background jobs; retain this synthetic outstanding
    // preparation by temporarily marking the source as not yet prepared.
    let source = stage.prepared.take();
    worker
        .request_attachment_send(channel, "original".into(), None, false)
        .unwrap();
    worker
        .stage_clipboard(
            channel,
            "next.bin".into(),
            "application/octet-stream".into(),
            Arc::new(vec![2]),
        )
        .unwrap();
    worker.state.set_draft(channel, "next draft".into());
    worker.cancel_attachment_send(channel);
    assert!(cancel.is_cancelled());
    assert!(!worker.attachments.sends.contains_key(&channel));
    assert_eq!(worker.state.drafts[&channel], "next draft");
    assert_eq!(worker.attachments.channels[&channel].len(), 2);
    assert_eq!(worker.attachments.channels[&channel][0].dto.id, id);
    assert_eq!(
        worker.attachments.channels[&channel][0].dto.status,
        AttachmentStatus::Failed
    );
    assert!(events.try_iter().any(|event|matches!(event,Event::MessageOperationFailed{channel_id,content,..}if channel_id==channel && content=="original")));
    drop(source);
    worker.clear_attachments();
}

#[tokio::test]
async fn invisible_only_messages_fail_locally_but_attachment_only_send_is_accepted() {
    let (mut worker, _) = demo();
    let channel = Snowflake(11);
    assert!(
        worker
            .request_attachment_send(channel, "\u{200b}\u{2060}".into(), None, false)
            .is_err()
    );
    worker
        .stage_clipboard(
            channel,
            "bytes.bin".into(),
            "application/octet-stream".into(),
            Arc::new(vec![1]),
        )
        .unwrap();
    settle_preparation(&mut worker, channel).await;
    worker
        .request_attachment_send(channel, String::new(), None, false)
        .unwrap();
    assert!(worker.attachments.sends.contains_key(&channel));
    worker.clear_attachments();
}
