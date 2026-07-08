// P1 milestone corpus: the core language subset, one file.

var counter:int = 0;
const GREETING:String = "hello,\tworld";
var anything:* = null;
var maybe:Number;

function fib(n:int):int {
    if (n < 2)
        return n;
    return fib(n - 1) + fib(n - 2);
}

function greet(name:String = "world", ...rest):void {
    trace("hello " + name);
}

function classify(value:Number):String {
    var out:String = "";
    switch (value % 3) {
        case 0:
            out = "fizz";
            break;
        case 1:
        case 2:
            out = "other";
            break;
        default:
            out = "impossible";
    }
    return out;
}

var totals:Vector.<Vector.<int>> = null;
var flags:Boolean = counter is int && !(GREETING as String == null);

outer: for (var i:int = 0; i < 10; i++) {
    for (var key:String in {a: 1, "b": 2, 3: [1, , 2.5e2]}) {
        if (key == "a")
            continue outer;
        break outer;
    }
}

for each (var item:* in [0x10, .5, 4294967295]) {
    counter += item;
}

do {
    counter--;
} while (counter > 0 && counter !== 1);

try {
    throw "boom";
} catch (e:String) {
    trace(e);
} catch (e) {
    trace("other", e);
} finally {
    counter = 0;
}

var adder:Function = function (a:int, b:int):int {
    return a + b;
};

counter &&= adder(1, 2);
counter ||= fib(5);
counter >>>= 1;
maybe = counter > 5 ? adder(counter, 1) : -counter;

var box = new Widget(counter)[0].size;
delete anything.stale;
trace(typeof maybe, classify(7), void 0);
