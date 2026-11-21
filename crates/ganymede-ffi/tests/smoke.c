/* Links the static library through include/ganymede.h, as the Swift shell
 * does: start, print the URL, register a kernel, stop. CI builds and runs
 * it. */
#include <stdio.h>
#include <string.h>
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
static void wrote(void *ctx, const char *path, const char *before,
                  const char *after) {
    (void)ctx; (void)path; (void)before; (void)after;
}
static int wrote_released = 0;
static void wrote_release(void *ctx) {
    (*(int *)ctx)++;
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
    const char *status =
        "{\"header\": {\"msg_id\": \"m\", \"msg_type\": \"status\", "
        "\"username\": \"\", \"session\": \"s\", "
        "\"date\": \"2026-01-01T00:00:00Z\", \"version\": \"5.3\"}, "
        "\"parent_header\": {}, \"metadata\": {}, "
        "\"content\": {\"execution_state\": \"idle\"}}";
    if (ganymede_kernel_deliver(server, "no-such-kernel", "iopub", status) != 2) {
        fprintf(stderr, "delivered to a kernel that isn't running\n");
        return 1;
    }
    ganymede_kernel_ended(server, "no-such-kernel", "lost");
    /* A machine mounts and unmounts; a bad id is refused with a message. */
    if (ganymede_server_add_machine(server, "mini", "http://127.0.0.1:1/", NULL, &err) != 0) {
        fprintf(stderr, "add_machine failed: %s\n", err);
        ganymede_string_free(err);
        return 1;
    }
    char *machines = ganymede_server_machines(server);
    if (!machines || strcmp(machines, "[\"mini\"]") != 0) {
        fprintf(stderr, "machines listed %s\n", machines ? machines : "NULL");
        return 1;
    }
    ganymede_string_free(machines);
    if (ganymede_server_add_machine(server, "a/b", "http://127.0.0.1:1/", "t", &err) == 0) {
        fprintf(stderr, "added a machine with a bad id\n");
        return 1;
    }
    ganymede_string_free(err);
    if (ganymede_server_remove_machine(server, "mini") != 1
        || ganymede_server_remove_machine(server, "mini") != 0) {
        fprintf(stderr, "remove_machine did not say whether it was mounted\n");
        return 1;
    }
    machines = ganymede_server_machines(server);
    if (!machines || strcmp(machines, "[]") != 0) {
        fprintf(stderr, "machines listed %s after the remove\n", machines ? machines : "NULL");
        return 1;
    }
    ganymede_string_free(machines);
    /* The write observer is set, replaced and removed. */
    ganymede_server_set_write_observer(server, wrote, wrote_release, &wrote_released);
    ganymede_server_set_write_observer(server, wrote, wrote_release, &wrote_released);
    if (wrote_released != 1) {
        fprintf(stderr, "write observer released %d times after a replace\n", wrote_released);
        return 1;
    }
    ganymede_server_set_write_observer(server, NULL, NULL, NULL);
    if (wrote_released != 2) {
        fprintf(stderr, "write observer released %d times after a remove\n", wrote_released);
        return 1;
    }
    ganymede_server_stop(server);
    if (released != 1) {
        fprintf(stderr, "release ran %d times\n", released);
        return 1;
    }
    return 0;
}
