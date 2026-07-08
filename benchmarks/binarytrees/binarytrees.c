#include <stdio.h>
#include <stdlib.h>
typedef struct Node { struct Node *left, *right; } Node;
static Node *make(int depth) {
    Node *n = malloc(sizeof(Node));
    if (depth == 0) { n->left = n->right = NULL; }
    else { n->left = make(depth - 1); n->right = make(depth - 1); }
    return n;
}
static int check(Node *n) {
    int total = 1;
    if (n->left) total += check(n->left);
    if (n->right) total += check(n->right);
    return total;
}
static void freeTree(Node *n) {
    if (!n) return;
    freeTree(n->left); freeTree(n->right); free(n);
}
int main(void) {
    int maxDepth = 16;
    Node *stretch = make(maxDepth + 1);
    printf("stretch: %d\n", check(stretch));
    freeTree(stretch);
    Node *longLived = make(maxDepth);
    long sum = 0;
    for (int d = 4; d <= maxDepth; d += 2) {
        int n = 1 << (maxDepth - d + 4);
        for (int i = 0; i < n; i++) {
            Node *t = make(d);
            sum += check(t);
            freeTree(t);
        }
    }
    printf("sum: %ld\n", sum);
    printf("long: %d\n", check(longLived));
    return 0;
}
