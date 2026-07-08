class Box.<T> {
    private var _value:T;
    public function Box(v:T) { _value = v; }
    public function get value():T { return _value; }
    public function set value(v:T):void { _value = v; }
    public function describe():String { return "box(" + _value + ")"; }
}

var bi:Box.<int> = new Box.<int>(42);
var bs:Box.<String> = new Box.<String>("hi");
bi.value = bi.value + 1;
trace(bi.value, bs.value, bi.describe());
trace(bi is Box.<int>, bi is Box.<String>);

var v:Vector.<int> = new <int>[1, 2, 3];
v.push(4);
v[0] = v[0] + 10;
trace(v.length, v[0], v.join("-"), v.indexOf(3));

var a:Array = [1, "two", 3.5];
a.push(true);
trace(a.length, a[1], a.join(","));
var parts:Array = "x,y,z".split(",");
trace(parts.length, parts[2]);

function sum(...nums):Number {
    var acc:Number = 0;
    for (var i:int = 0; i < nums.length; i++)
        acc += nums[i];
    return acc;
}
trace(sum(1, 2, 3.5));
