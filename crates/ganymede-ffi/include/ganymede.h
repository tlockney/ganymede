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
