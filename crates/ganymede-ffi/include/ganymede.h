/* Run the Ganymede server inside an app. See crates/ganymede-ffi/src/lib.rs
 * for the full contract. Strings are UTF-8. */
#ifndef GANYMEDE_H
#define GANYMEDE_H

#ifdef __cplusplus
extern "C" {
#endif

typedef struct GanymedeServer GanymedeServer;

/* Starts a server on loopback and returns once it listens, or after 30
 * seconds with an error if it has not. config_json is
 * a JSON object with ganymede.toml's keys, or NULL. Returns NULL on
 * failure and sets *error_out (if error_out is not NULL) to a message to
 * free with ganymede_string_free. */
GanymedeServer *ganymede_server_start(const char *config_json, char **error_out);

/* The URL with its token. Owned by the server; valid until it stops. */
const char *ganymede_server_url(const GanymedeServer *server);

/* Shuts down kernels, stops the server and frees it. NULL is ignored.
 * Blocks until done: with registered kernels whose callbacks need the main
 * thread, call it off the main thread, or each such kernel waits out the
 * shutdown grace period before it is stopped. */
void ganymede_server_stop(GanymedeServer *server);

/* Mounts the Ganymede server at url (http(s)://host[:port][/base]) as
 * machine id (1 to 64 of A-Z a-z 0-9 _ -): the app's own pages' requests to
 * /machines/{id}/... go to it, HTTP and websocket, an allowlist of routes
 * only (#894). token (may be NULL) is added to each request. Every request
 * also sends a Ganymede-Client header, which a machine that lets this
 * device in by tailnet identity accepts, so a NULL token can work there. The page's own cookie, token and Origin are
 * never forwarded, and the machine's token is never given to the page.
 * Nothing is mounted until this is called. No request is made now. An id
 * already mounted is replaced, and its open websockets closed. Any thread.
 * Returns 0, or nonzero on failure (a bad id or URL): then *error_out (if
 * error_out is not NULL) is a message to free with ganymede_string_free. */
int ganymede_server_add_machine(GanymedeServer *server, const char *id,
                                const char *url, const char *token,
                                char **error_out);

/* Unmounts machine id and closes its open websockets. Returns 1 if it was
 * mounted, 0 if not. Any thread. */
int ganymede_server_remove_machine(GanymedeServer *server, const char *id);

/* Called after the server writes a file. path is the file's canonical
 * absolute path (links resolved; \\?\C:\... on Windows). before and after
 * are its stamp (size, modification time in ns and, on Unix, inode: the same
 * as file_stamp in crates/ganymede-contents/src/fileid.rs) just before and
 * just after the write, or NULL when it can't be read (before, for a new
 * file). Strings are valid during the call. */
typedef void (*GanymedeWriteCallback)(void *ctx, const char *path,
                                      const char *before, const char *after);

/* Calls callback(ctx, ...) once after each write the server makes of a file
 * under its root (#916): a REST save, a room's autosave, a run saving its
 * outputs, MCP, any successful save of a regular file (a new one's
 * included, before NULL), a checkpoint restore, and a copy to a named
 * destination path. Not for a
 * write that failed, a copy into a directory, a new untitled file, a
 * rename, a delete, a directory save, or when the root isn't a local
 * directory. Lets the app tell its
 * server's writes from a file replaced outside, to keep or forget what it
 * keeps per file. The callback runs on a server thread, never the main
 * thread, maybe at once for different files; it must not block. ctx must be
 * usable from any thread until the observer is replaced or the server
 * stopped (a call already under way may still use the old ctx when this
 * returns). A path with a NUL byte, or that is not UTF-8, is not reported.
 * Replaces the observer set before: a write is reported to whatever is set
 * when it ends, and one that began while none was set is not reported.
 * NULL callback removes it (and release is then ignored). release (may be
 * NULL) is called with ctx once, after the last call that uses that ctx has
 * returned and none follows: when the observer is replaced or removed, or
 * the server stops. The app frees ctx there, and not before. It runs on a
 * server thread or on the thread of this call or of ganymede_server_stop,
 * and must not block. Any thread. */
void ganymede_server_set_write_observer(GanymedeServer *server,
                                        GanymedeWriteCallback callback,
                                        void (*release)(void *ctx),
                                        void *ctx);

/* Frees a string returned by this library. NULL is ignored. */
void ganymede_string_free(char *s);

/* An app's callbacks for kernels it runs itself (#266). The server calls
 * start, send and stop on its own threads, never the main thread, maybe at
 * once for different kernels; they must not block. send calls for one
 * kernel don't overlap and come in order. Strings are valid during the
 * call. release (may be NULL) runs once, when the server stops, after
 * every other callback. */
typedef struct GanymedeKernelCallbacks {
    void *ctx;
    /* launch_json: {"kernelspec", "cwd", "env"}; env has JPY_SESSION_NAME,
     * the notebook's path. Return 0 once the start is under way. */
    int (*start)(void *ctx, const char *kernel_id, const char *launch_json);
    /* A Jupyter message as JSON (buffers as base64 strings) on "shell",
     * "control" or "stdin". */
    void (*send)(void *ctx, const char *kernel_id, const char *channel,
                 const char *msg_json);
    /* End the kernel now. A restart then calls start with the same
     * kernel_id, so once stop is called (or the app called
     * ganymede_kernel_ended), deliver nothing more from that run. */
    void (*stop)(void *ctx, const char *kernel_id);
    void (*release)(void *ctx);
} GanymedeKernelCallbacks;

/* Lists spec_json (kernel.json's keys plus "name", lowercased, of
 * [a-z0-9._-]) as a kernelspec run by callbacks, after the server's other
 * kernels and in place of one of the same name, so the default kernel
 * stays the server's own unless it has none. Returns 0, or nonzero on
 * failure (a name already registered, among others): then no callback is
 * ever called, release included, and *error_out (if error_out is not
 * NULL) is a message to free with ganymede_string_free. */
int ganymede_kernel_register(GanymedeServer *server, const char *spec_json,
                             const GanymedeKernelCallbacks *callbacks,
                             char **error_out);

/* A message from a registered kernel, as JSON, on "shell", "control",
 * "stdin" or "iopub". Any thread; doesn't block. Returns 0; 1 when a string
 * is NULL or the message or channel can't be read (the app's
 * bug); 2 when the kernel isn't running (expected after a stop). */
int ganymede_kernel_deliver(GanymedeServer *server, const char *kernel_id,
                            const char *channel, const char *msg_json);

/* A registered kernel ended without being asked to. reason "lost" (the
 * platform ended it) restarts it, unless restarts are off, counted
 * against its own limit (3, each within 60 s of the last) and not as a crash, once it has
 * started (a loss before its first kernel_info_reply is a failed start,
 * retried as one); anything else, or NULL, is a crash. Any thread. Call
 * it once per run, never for a run already stopped. */
void ganymede_kernel_ended(GanymedeServer *server, const char *kernel_id,
                           const char *reason);

/* Called on the panicking thread with "panicked at <file>:<line>:<col>:
 * <message>", valid only during the call. */
typedef void (*GanymedePanicCallback)(const char *message);

/* Calls callback on every panic in this library, after printing it to
 * stderr, for the app's crash reporter. NULL removes it. */
void ganymede_set_panic_callback(GanymedePanicCallback callback);

/* Panics on the calling thread; the process aborts. For testing crash
 * reports. */
void ganymede_debug_panic(void);

/* 1 when the library was built with the v8-bench feature, which the
 * XCFramework is; 0 otherwise. */
int ganymede_v8_bench_available(void);

/* Runs the engine benchmark (#892) on V8 and returns the report as JSON, to
 * free with ganymede_v8_bench_free. config_json: {"jitless": bool,
 * "repeats": n, "workloads": [{"name": "...", "sources": ["js", ...]}]};
 * jitless defaults to true and repeats to 5. Each workload runs in a fresh
 * isolate; the report has per-workload start, load, first, steady and cold
 * times, the heap and the process's peak memory. A NULL or invalid config,
 * a panic, or a library without V8 gives {"error": "..."}; a workload whose
 * script throws has its own "error". Blocks until done (seconds), so call
 * it off the main thread; it runs on a thread of its own with a big stack,
 * one call at a time (a workload that never returns holds the lock for good,
 * and V8 running out of heap aborts the process). repeats is at most 1000.
 * V8's flags are set by the first call in a process: later calls keep its
 * "jitless", which the report shows. The iOS device library has no JIT and
 * always reports jitless true; the simulator's keeps it. In a Mac app under
 * the hardened runtime without the allow-jit entitlement, "jitless": false
 * aborts the process. Always exported. */
char *ganymede_v8_bench_run(const char *config_json);

/* Frees a string ganymede_v8_bench_run returned. NULL is ignored. */
void ganymede_v8_bench_free(char *s);

#ifdef __cplusplus
}
#endif

#endif /* GANYMEDE_H */
