#include <stdio.h>
#include <string.h>

struct hdr { int count; char name[16]; };

int g_marker = 0;

int parse_header(struct hdr *h, int remaining) {
    h->count = 0;
    if (remaining < 8) return -1;   /* early return leaves count = 0 */
    h->count = remaining / 8;
    return 0;
}

struct hdr *lookup(struct hdr *h) {
    if (h->count == 0) return NULL;
    return h;
}

int main(int argc, char **argv) {
    struct hdr h;
    g_marker = 0xdeadbeef;
    int remaining = argc > 1 ? (int)strlen(argv[1]) : 3;
    parse_header(&h, remaining);
    struct hdr *f = lookup(&h);
    printf("count=%d\n", f->count);   /* NULL deref */
    return 0;
}
