/* Run the Ganymede server inside an app. See crates/ganymede-ffi/src/lib.rs
 * for the full contract. Strings are UTF-8. */
#ifndef GANYMEDE_H
#define GANYMEDE_H

#ifdef __cplusplus
extern "C" {
#endif

typedef struct GanymedeServer GanymedeServer;

/* Starts a server on loopback and returns once it listens. config_json is
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

#ifdef __cplusplus
}
#endif

#endif /* GANYMEDE_H */
