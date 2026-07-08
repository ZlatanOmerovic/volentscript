// System.readLine(): uppercase every stdin line until EOF.
var line:String? = System.readLine();
while (line != null) {
    trace(line.toUpperCase());
    line = System.readLine();
}
trace("eof");
