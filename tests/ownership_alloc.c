/* Linux/glibc-only foreground allocation observer, not editor/runtime code.
 * cc -shared -fPIC -O2 tests/ownership_alloc.c -o /tmp/dust-ownership-alloc.so
 * LD_PRELOAD=/tmp/dust-ownership-alloc.so <lib-test-binary> window_admission_cost_probe --ignored --nocapture --test-threads=1
 * Counts successful allocation requests/bytes (NOT retained/peak memory), only
 * on the thread writing the START/END markers. Run latency probes without it.
 */
#define _GNU_SOURCE
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <sys/syscall.h>
extern void *__libc_malloc(size_t);
extern void *__libc_calloc(size_t, size_t);
extern void *__libc_realloc(void *, size_t);
extern void __libc_free(void *);
static _Thread_local int active;
static _Thread_local size_t calls, bytes;
static void record(void *p, size_t n) { if (active && p) { ++calls; bytes += n; } }
void *malloc(size_t n) { void *p=__libc_malloc(n); record(p,n); return p; }
void *calloc(size_t n,size_t size) { void *p=__libc_calloc(n,size); record(p,n*size); return p; }
void *realloc(void *old,size_t n) { void *p=__libc_realloc(old,n); record(p,n); return p; }
void free(void *p) { __libc_free(p); }
ssize_t write(int fd,const void *data,size_t len) {
    if (fd==2 && len>=16 && !memcmp(data,"DUST_ALLOC_START",16)) { calls=bytes=0; active=1; }
    if (fd==2 && len>=14 && !memcmp(data,"DUST_ALLOC_END",14)) {
        active=0;
        char output[128];
        int n=snprintf(output,sizeof output,"foreground_allocation_calls=%zu requested_bytes=%zu\n",calls,bytes);
        if (n>0 && (size_t)n<sizeof output) syscall(SYS_write,2,output,(size_t)n);
    }
    return syscall(SYS_write,fd,data,len);
}
