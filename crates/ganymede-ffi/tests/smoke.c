/* Links the static library through include/ganymede.h, as the Swift shell
 * does: start, print the URL, stop. CI builds and runs it. */
#include <stdio.h>
#include "ganymede.h"

int main(void) {
    char *err = NULL;
    GanymedeServer *server = ganymede_server_start("{\"root_dir\": \".\"}", &err);
    if (!server) {
        fprintf(stderr, "start failed: %s\n", err);
        ganymede_string_free(err);
        return 1;
    }
    printf("%s\n", ganymede_server_url(server));
    ganymede_server_stop(server);
    return 0;
}
