public class strings {
    public static void main(String[] a) {
        String text = "the quick brown fox jumps over the lazy dog";
        long checksum = 0;
        for (int i = 0; i < 60000; i++) {
            String joined = String.join("-", text.split(" "));
            checksum += joined.indexOf("fox");
            checksum += joined.toUpperCase().length();
            checksum += joined.replaceFirst("quick", "slow").lastIndexOf("o");
        }
        System.out.println(checksum);
    }
}
