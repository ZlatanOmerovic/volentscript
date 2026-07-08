// RegExp milestone (SPECS §6, ES3 §15.10): literals, flags, test/exec,
// String.match/search/replace, lastIndex, error paths, is/as, GC churn.

var re:RegExp = /ab+c/i;
trace(re);
trace(re.source + " | " + re.global + " " + re.ignoreCase + " " + re.multiline);
trace(re.test("xxABBBCzz") + " " + re.test("nope"));

var d:RegExp = new RegExp("\\d+", "g");
trace(d.test("a1b22c333") + " lastIndex=" + d.lastIndex);
var m:Array? = d.exec("a1b22c333");
if (m != null) {
    trace("exec " + m[0] + " lastIndex=" + d.lastIndex);
}

var all:Array? = "a1b22c333".match(/\d+/g);
trace("match " + all);
trace("search " + "hello world".search(/wor/));
trace("replace " + "2026-07-08".replace(/(\d+)-(\d+)-(\d+)/, "$3/$2/$1"));
trace("replace-g " + "a-b-c".replace(/-/g, "+"));
trace("replace-str " + "a-b-c".replace("-", "+"));

var g:Array? = /(\w+)@(\w+)/.exec("mail zed@example now");
if (g != null) {
    trace("groups " + g[1] + " " + g[2]);
}

// backreference + lazy quantifier (the reason for a backtracking engine)
trace("backref " + /(\w)\1/.test("balloon"));
trace("lazy " + "<a><b>".replace(/<.+?>/, "_"));

var boxed:* = /pi+ng/;
trace("is " + (boxed is RegExp) + " " + (boxed is String));
var back:RegExp? = boxed as RegExp;
if (back != null) {
    trace("as " + back.test("piiing"));
}

try {
    var bad:RegExp = new RegExp("(unclosed", "");
    trace("unreached");
} catch (e:SyntaxError) {
    trace("caught " + e.name);
}

// churn: compiled programs are GC blocks with destructors
for (var i:int = 0; i < 50000; i++) {
    var t:RegExp = new RegExp("x" + (i % 7), "");
    t.test("xxx");
}
System.gc();
trace("bounded " + (System.gcLiveBytes() < 2000000));
trace("division " + (10 / 2));
trace("regex done");
