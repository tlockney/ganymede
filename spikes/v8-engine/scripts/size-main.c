/* A stand-in app for scripts/ios-size.sh: links the spike's library and
 * calls it, so the linker keeps what an engine would use. */
#include <stdio.h>
#include "../include/v8_spike.h"

int main(int argc, char **argv) {
    const char *config = argc > 1 ? argv[1]
        : "{\"workloads\":[{\"name\":\"start\",\"sources\":[\"globalThis.workload={run:()=>String(1+1)}\"]}]}";
    char *report = v8_spike_run(config);
    puts(report);
    v8_spike_free(report);
    return v8_spike_has_v8() ? 0 : 1;
}
