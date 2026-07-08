// GC milestone (SPECS §7): churn far more garbage than the collection
// threshold, keep a few survivors, and verify (a) survivors are intact
// after collection and (b) the live set stays bounded.

var keep:Array = [];

function makeRow(i:int):Object {
    var label:String = "payload-" + i + "-" + (i * 2);
    var row:Object = { name: label, idx: i, halves: [label, i, i * 0.5] };
    return row;
}

for (var i:int = 0; i < 200000; i++) {
    var row:Object = makeRow(i);
    var copy:Array = [row.name, row.idx];
    if (i % 20000 == 0) {
        keep.push(copy[0]);
    }
}

// Closures capture cells; churn those too.
var acc:int = 0;
for (var j:int = 0; j < 50000; j++) {
    var n:int = j;
    var f:Function = function():int { return n + 1; };
    acc = f();
}

System.gc();
trace("survivors = " + keep.length);
trace("first = " + keep[0]);
trace("last = " + keep[9]);
trace("acc = " + acc);
trace("bounded = " + (System.gcLiveBytes() < 8000000));
trace("gc ok");
