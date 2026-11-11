/* The engine benchmark's C entry points (spikes/v8-engine, #646). */
#ifndef V8_SPIKE_H
#define V8_SPIKE_H

/* Runs a benchmark. config_json: {"jitless": bool, "repeats": n,
 * "workloads": [{"name": "...", "sources": ["js", ...]}]}. Returns the
 * report as JSON, which v8_spike_free frees. V8's flags are set by the
 * first call: later calls keep its "jitless". Not thread safe. */
char *v8_spike_run(const char *config_json);

/* Returns 1 when the library was built with V8, 0 for the size baseline. */
int v8_spike_has_v8(void);

void v8_spike_free(char *s);

#endif
