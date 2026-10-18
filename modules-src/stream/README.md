# stream

Streaming asset system. Textures stream in on a worker thread and are evicted
LRU when the resident set exceeds the budget -- nothing is force-loaded.

## Lua surface
`stream.load(path)`, `stream.prefetch{...}`, `stream.release(path)`,
`stream.set_budget(mb)`, `stream.stats()` -> `{loaded, loading, resident_mb,
budget_mb, evictions, loads}`.
