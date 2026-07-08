class Node {
  constructor(left, right) { this.left = left; this.right = right; }
}
function make(depth) {
  return depth === 0 ? new Node(null, null) : new Node(make(depth - 1), make(depth - 1));
}
function check(n) {
  let total = 1;
  if (n.left) total += check(n.left);
  if (n.right) total += check(n.right);
  return total;
}
const maxDepth = 16;
console.log("stretch: " + check(make(maxDepth + 1)));
const longLived = make(maxDepth);
let sum = 0;
for (let d = 4; d <= maxDepth; d += 2) {
  const n = 1 << (maxDepth - d + 4);
  for (let i = 0; i < n; i++) sum += check(make(d));
}
console.log("sum: " + sum);
console.log("long: " + check(longLived));
