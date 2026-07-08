namespace red;
namespace blue = "demo:blue";
namespace crimson = "demo:blue";   // same URI => same namespace as blue

class Signal {
    public var label:String = "sig";
    red var tint:String = "warm";
    red function describe():String { return "red " + label; }
    blue function describe():String { return "blue " + label; }
    public function describe():String { return "plain " + label; }
}

class Alarm extends Signal {
    red override function describe():String { return "RED " + label; }
}

var s:Signal = new Signal();
trace(s.describe());
trace(s.red::describe());
trace(s.blue::describe());
trace(s.crimson::describe());
trace(s.red::tint);
s.red::tint = "hot";
trace(s.red::tint);

var a:Signal = new Alarm();
trace(a.red::describe());   // virtual dispatch through namespace

use namespace red;
trace(s.tint);              // unqualified via open namespace
trace(s.describe());        // public still wins (exact match first)
trace("ns done");
