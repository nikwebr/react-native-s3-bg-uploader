#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

#define MAX_CONCURRENT_UPLOADS 4

#define MAX_RETRIES 3

typedef struct Option_ProgressCallback Option_ProgressCallback;

#ifdef __cplusplus
extern "C" {
#endif

int32_t add(int32_t one, int32_t two);

int32_t upload_file(const char *path);

double get_upload_progress(void);

const char *get_upload_progress_json(void);

void set_progress_callback(struct Option_ProgressCallback callback);

#ifdef __cplusplus
}
#endif
