# ADR-0103: Persist user attachments as Session Artifacts

Status: Accepted

## Decision

User attachments are explicit turn input, independent of workspace read Tools.
The Agent Loop validates bounded inline PNG/JPEG/WebP images and UTF-8 text,
then stores their immutable bytes through the selected Artifact Plugin before
recording the Turn. Session events and retained compaction messages contain
only typed attachment metadata and opaque Artifact handles. Original local
paths and browser object URLs are never durable identifiers.

The Model Capability accepts optional native image content alongside text.
Providers must reject unsupported images rather than silently discard them.
Text attachments are decoded and bounded when constructing model context.
Compaction retains attachment references with retained messages; older images
may leave the model context while remaining visible in durable history.
Missing or expired Artifacts fail explicitly. Artifact retention remains the
existing storage owner's policy; this feature does not promise infinite retention.

Surfaces accept inline bytes, never arbitrary server filesystem paths or remote
URLs. Reusing an attachment requires a reference present in the source Session;
edited turns snapshot reused bytes into the forked Session. The Web surface
serves only attachments referenced by that Session through the Artifact API.
Web and mini Agent share composer and history presentation components.

First-release bounds: at most four attachments, 2 MiB per image, 128 KiB per
text file, 8 MiB total decoded. Images are dimension-checked before admission.
Attachment-only turns are supported. Existing text-only inputs remain valid.
The optional contract additions advance minor versions and generated projections
are regenerated from their authoring source.

## Ownership and removal

Artifact owns bytes, capacity and retention; Session owns attachment references;
Agent Loop owns admission and context assembly; Model Providers own wire mapping;
Web owns authenticated transport; Console owns transient selection and previews.
No new Plugin identity, private storage bypass, or Kernel policy is introduced.
