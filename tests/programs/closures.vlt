// closures
function makeCounter(start:int):Function {
    var count:int = start;
    return function():int {
        count++;
        return count;
    };
}
var c1:Function = makeCounter(10);
var c2:Function = makeCounter(100);
trace(c1(), c1(), c2(), c1());

// method closure binds this permanently (SPECS §3.7)
class Greeter {
    private var _who:String;
    public function Greeter(who:String) { _who = who; }
    public function hello():String { return "hi " + _who; }
}
var g:Greeter = new Greeter("zlatan");
var m:Function = g.hello;
trace(m(), m.call(null), m.apply(null, []));

// sort comparator
var nums:Array = [5, 1, 4, 2, 3];
nums.sort(function(a:*, b:*):Number { return a - b; });
trace(nums.join(","));

// for each / for in
var total:Number = 0;
for each (var n:Number in nums)
    total += n;
var keys:String = "";
for (var k:int in new <int>[7, 8, 9])
    keys += k;
trace(total, keys);

// exceptions
function risky(mode:int):String {
    try {
        if (mode == 0)
            throw new TypeError("custom boom");
        if (mode == 1) {
            var v:Vector.<int> = new <int>[1];
            return "" + v[5];              // runtime RangeError
        }
        return "clean";
    } catch (e:TypeError) {
        return "caught-type: " + e.message;
    } catch (e:Error) {
        return "caught-error: " + e.message;
    } finally {
        trace("finally", mode);
    }
    return "unreachable";
}
trace(risky(0));
trace(risky(1));
trace(risky(2));

// throw across functions + catch-all + toString of Error
function deep():void { throw new ArgumentError("from deep"); }
try {
    deep();
} catch (e) {
    trace("caught:", e);
}
trace("done");
