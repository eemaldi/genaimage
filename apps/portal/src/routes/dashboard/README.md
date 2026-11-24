# Dashboard data

The health endpoint probes ComfyUI on request; `backend_health_changed` is
published when a probe observes a change, not by an independent monitor. The
dashboard polls health every 30 seconds while visible and on visibility return.
Events matching the latest snapshot require no probe; other events invalidate
it. Invalidations during a probe are reconciled afterwards and event-triggered
probes are coalesced and limited to one every five seconds. Resync replaces
in-flight requests. Service recovery hints come from the health response;
older servers may omit them. Problem responses supply their own recovery hints.

The runs API has no aggregate count endpoint. The dashboard reads one page of
at most 50 recent runs and keeps live state and buffered updates within that
limit. When older records exist, each status count is a lower bound marked `+`,
not a total or a transactional point-in-time database snapshot. Output events
and completed runs refresh the latest indexed outputs. Reconnects without a
replay cursor and server resync signals trigger new snapshots. While the event
stream is disconnected, displayed activity may be out of date.
