# Transfer center

The transfer center is an explicit, persistent queue for sending a history entry to one selected Tailscale device or downloading the files in a Files history entry. Creating a send without `entry_id` captures the clipboard exactly once as part of that user request. Retries use the saved entry snapshot and never read the clipboard.

`POST /v1/transfers` accepts only `operation`, `node_id`, `entry_id`, and `expires_in_secs`. Send requires a discovered `node_id`; an offline device may be queued and is resolved again by exact Tailscale ID for each attempt. Download requires an existing Files entry and obtains payloads from the verified source device. An entry whose source is this device or has no source identity is rejected; use the original file locally. Requests return `202` with a job. `GET /v1/transfers` returns at most 128 persisted jobs; detail, cancel, retry, and terminal-record deletion are available under `/v1/transfers/{id}`. TTL defaults to 24 hours and may be 60 seconds through 7 days.

A completed send means that the selected peer accepted the entry metadata and inline content into its history. It does not mean that file payloads were downloaded. A completed download means the listed files were saved under the configured download directory; it does not apply anything to the clipboard or open the files. The source files must remain available on the source machine, and that machine must be online during a download.

Jobs are stored separately from clipboard history, bounded to 128 records, restored after restart, and expire according to their TTL. Offline waits use bounded exponential backoff. Integrity, authorization, and invalid-source failures terminate for explicit user retry. Downloads preserve byte-accurate progress and HTTP range resume data; cancellation leaves resumable partial data but never reports a completed file unless its output exists.

Cancelling stops future attempts and aborts an active file stream cooperatively. A send that the peer has already accepted cannot be recalled. Files already completed by a download remain in the download directory and stay marked complete if the rest of the job is cancelled.
