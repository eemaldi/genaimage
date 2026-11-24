# Dashboard data

The health endpoint probes ComfyUI on request; `backend_health_changed` is
published when a probe observes a change, not by an independent monitor. The
dashboard reacts to these events and polls health every 30 seconds while the
document is visible, including when it becomes visible again. An in-flight
probe absorbs health events to avoid a probe/event feedback loop. A resync
replaces the in-flight probe. The health response currently has no endpoint or
recovery fields; problem responses display their supplied recovery guidance.

The runs API has no aggregate count endpoint. The dashboard reads all pages to
count recorded runs by status, and buffers run events while loading. This can
be expensive for large histories; the counts reflect the latest snapshots and
received events, not a transactional point-in-time database snapshot. Output
events and completed runs trigger a refresh of the latest indexed outputs.
A reconnect without a replay cursor and server resync signals trigger new
snapshots. While the event stream is disconnected, displayed activity may be
out of date.
