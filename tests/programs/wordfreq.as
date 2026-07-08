// A real CLI tool (P7 milestone): word frequency -> JSON report.

var argv:Array = System.args();
if (argv.length < 2) {
    trace("usage: wordfreq <input> <output.json>");
    System.exit(2);
}
var inputPath:String = "" + argv[0];
var outputPath:String = "" + argv[1];

if (!File.exists(inputPath)) {
    trace("no such file:", inputPath);
    System.exit(1);
}
var text:String? = File.read(inputPath);
if (text == null) {
    trace("cannot read:", inputPath);
    System.exit(1);
} else {
    var words:Array = text.toLowerCase().replace(",", " ").split(" ");
    var counts:* = {};
    var order:Array = [];
    for each (var w:String in words) {
        if (w == "" ) continue;
        if (w in counts) {
            counts[w] = counts[w] + 1;
        } else {
            counts[w] = 1;
            order.push(w);
        }
    }
    order.sort(function(a:*, b:*):Number { return counts[b] - counts[a]; });

    var top:Array = order.slice(0, Math.min(3, order.length));
    var lines:Array = top.map(function(w:*, i:*, arr:*):* {
        return { word: w, count: counts[w] };
    });
    var report:* = {
        file: inputPath,
        total: words.filter(function(x:*, i:*, a:*):* { return x != ""; }).length,
        unique: order.length,
        top: lines
    };
    var json:String = JSON.stringify(report);
    File.write(outputPath, json);
    trace("wrote", outputPath);

    // read back through JSON.parse to prove the round trip
    var parsed:* = JSON.parse("" + File.read(outputPath));
    trace("total:", parsed.total, "unique:", parsed.unique);
    for each (var entry:* in parsed.top)
        trace(entry.word, "=", entry.count);
    trace("sqrt(unique) ~", Math.round(Math.sqrt(parsed.unique)));
}
