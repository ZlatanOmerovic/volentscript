/* C has no string stdlib comparable to the others; this mirrors the same
   operations with hand-rolled helpers over heap buffers. */
#include <ctype.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
static char *join_dash(const char *s) {
    char *out = strdup(s);
    for (char *p = out; *p; p++)
        if (*p == ' ') *p = '-';
    return out;
}
static long last_index_of(const char *s, char c) {
    long last = -1;
    for (long i = 0; s[i]; i++)
        if (s[i] == c) last = i;
    return last;
}
static char *replace_first(const char *s, const char *from, const char *to) {
    const char *hit = strstr(s, from);
    if (!hit) return strdup(s);
    size_t pre = (size_t)(hit - s);
    char *out = malloc(strlen(s) - strlen(from) + strlen(to) + 1);
    memcpy(out, s, pre);
    strcpy(out + pre, to);
    strcat(out, hit + strlen(from));
    return out;
}
int main(void) {
    const char *text = "the quick brown fox jumps over the lazy dog";
    long checksum = 0;
    for (int i = 0; i < 60000; i++) {
        char *joined = join_dash(text);
        const char *fox = strstr(joined, "fox");
        checksum += fox ? (long)(fox - joined) : -1;
        char *upper = strdup(joined);
        for (char *p = upper; *p; p++) *p = (char)toupper((unsigned char)*p);
        checksum += (long)strlen(upper);
        char *replaced = replace_first(joined, "quick", "slow");
        checksum += last_index_of(replaced, 'o');
        free(joined); free(upper); free(replaced);
    }
    printf("%ld\n", checksum);
    return 0;
}
