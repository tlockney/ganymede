/* Links the static library through include/ganymede.h, as the Swift shell
 * does: start, print the URL, register a kernel, stop. CI builds and runs
 * it. */
#include <stdio.h>
#include "ganymede.h"

static int released = 0;

static int start(void *ctx, const char *kernel_id, const char *launch_json) {
    (void)ctx; (void)kernel_id; (void)launch_json;
    return 0;
}
static void send_msg(void *ctx, const char *kernel_id, const char *channel,
                     const char *msg_json) {
    (void)ctx; (void)kernel_id; (void)channel; (void)msg_json;
}
static void stop(void *ctx, const char *kernel_id) {
    (void)ctx; (void)kernel_id;
}
static void release(void *ctx) {
    (*(int *)ctx)++;
}

int main(void) {
    char *err = NULL;
    GanymedeServer *server = ganymede_server_start("{\"root_dir\": \".\"}", &err);
    if (!server) {
        fprintf(stderr, "start failed: %s\n", err);
        ganymede_string_free(err);
        return 1;
    }
    printf("%s\n", ganymede_server_url(server));
    GanymedeKernelCallbacks callbacks = {&released, start, send_msg, stop, release};
    const char *spec = "{\"name\": \"smoke\", \"display_name\": \"Smoke\", \"language\": \"typescript\"}";
    if (ganymede_kernel_register(server, spec, &callbacks, &err) != 0) {
        fprintf(stderr, "register failed: %s\n", err);
        ganymede_string_free(err);
        return 1;
    }
    /* A second registration of the name fails and never calls release. */
    if (ganymede_kernel_register(server, spec, &callbacks, &err) == 0) {
        fprintf(stderr, "registered the same name twice\n");
        return 1;
    }
    ganymede_string_free(err);
    if (ganymede_kernel_deliver(server, "no-such-kernel", "iopub", "{}") == 0) {
        fprintf(stderr, "delivered to a kernel that isn't running\n");
        return 1;
    }
    ganymede_kernel_ended(server, "no-such-kernel", "lost");
    ganymede_server_stop(server);
    if (released != 1) {
        fprintf(stderr, "release ran %d times\n", released);
        return 1;
    }
    return 0;
}
