namespace red;
namespace blue = "demo:blue";

class Signal {
    public var label:String = "sig";
    red var tint:String = "warm";
    red function describe():String { return "red " + label; }
    blue function describe():String { return "blue " + label; }
}
class Alarm extends Signal {
    red override function describe():String { return "RED " + label; }
}

// Namespace values
var q:Namespace = red;
var b:Namespace = blue;
trace(q.uri);
trace(b.uri);
trace(b == new Namespace("demo:blue"));   // URI identity via interning
trace(q == b);
trace("" + b);                            // toString = uri
trace(typeof q);
var boxed:* = q;
trace(boxed is Namespace);

// Runtime-computed qualification
var s:Signal = new Signal();
trace(s.q::describe());
trace(s.b::describe());
trace(s.q::tint);

// virtual dispatch through runtime qualifier
var a:Signal = new Alarm();
trace(a.q::describe());

// method as value through runtime qualifier
var f:* = s.q::describe;
trace(f());

// missing member -> catchable ReferenceError
try {
    var ghost:Namespace = new Namespace("nope");
    s.ghost::describe();
} catch (e:ReferenceError) {
    trace("caught: " + e.message);
}
trace("nsval done");
