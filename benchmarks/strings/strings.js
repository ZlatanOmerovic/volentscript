const text = "the quick brown fox jumps over the lazy dog";
let checksum = 0;
for (let i = 0; i < 60000; i++) {
  const joined = text.split(" ").join("-");
  checksum += joined.indexOf("fox");
  checksum += joined.toUpperCase().length;
  checksum += joined.replace("quick", "slow").lastIndexOf("o");
}
console.log(checksum);
