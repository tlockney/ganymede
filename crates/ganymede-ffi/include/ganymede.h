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

/* Shuts down kernels, stops the server and frees it. NULL is ignored. */
void ganymede_server_stop(GanymedeServer *server);

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
    /* End the kernel now. */
    void (*stop)(void *ctx, const char *kernel_id);
    void (*release)(void *ctx);
} GanymedeKernelCallbacks;

/* Lists spec_json (kernel.json's keys plus "name") as a kernelspec run by
 * callbacks, before the server's other kernels. Returns 0, or nonzero on
 * failure (a name already registered, among others): then no callback is
 * ever called, release included, and *error_out (if error_out is not
 * NULL) is a message to free with ganymede_string_free. */
int ganymede_kernel_register(GanymedeServer *server, const char *spec_json,
                             const GanymedeKernelCallbacks *callbacks,
                             char **error_out);

/* A message from a registered kernel, as JSON, on "shell", "control",
 * "stdin" or "iopub". Any thread; doesn't block. Returns 0, or nonzero
 * when the kernel isn't running or the message can't be read. */
int ganymede_kernel_deliver(GanymedeServer *server, const char *kernel_id,
                            const char *channel, const char *msg_json);

/* A registered kernel ended without being asked to. reason "lost" (the
 * platform ended it) restarts it without counting a crash; anything else,
 * or NULL, is a crash. Any thread. */
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

#ifdef __cplusplus
}
#endif

#endif /* GANYMEDE_H */
