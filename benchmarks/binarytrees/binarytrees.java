public class binarytrees {
    static class Node {
        Node left, right;
        Node(Node l, Node r) { left = l; right = r; }
    }
    static Node make(int depth) {
        return depth == 0 ? new Node(null, null) : new Node(make(depth - 1), make(depth - 1));
    }
    static int check(Node n) {
        int total = 1;
        if (n.left != null) total += check(n.left);
        if (n.right != null) total += check(n.right);
        return total;
    }
    public static void main(String[] a) {
        int maxDepth = 16;
        System.out.println("stretch: " + check(make(maxDepth + 1)));
        Node longLived = make(maxDepth);
        long sum = 0;
        for (int d = 4; d <= maxDepth; d += 2) {
            int n = 1 << (maxDepth - d + 4);
            for (int i = 0; i < n; i++) sum += check(make(d));
        }
        System.out.println("sum: " + sum);
        System.out.println("long: " + check(longLived));
    }
}
