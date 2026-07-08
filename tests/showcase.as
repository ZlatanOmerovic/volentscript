// showcase.as
// ASR (working name) — language capability showcase / end-to-end golden test.
// Conforms to SPECS.md. No Flash/SWF/AVM2 — pure language.
//
// PHASE NOTE: the truly-minimal slice that must run FIRST (Phase 3) is just the
// hello-world + trace lines in section (1) and the if/else in section (3). The
// rest comes online as Phases 4–7 land (classes, generics, closures, exceptions,
// stdlib). This whole file is the *final* golden test; the expected stdout is at
// the bottom.

package demo {

    // ---- Interface: method + accessor signatures (SPECS §3.5) ----
    public interface Shape {
        function area():Number;
        function get name():String;
    }

    // ---- Sealed-by-default class implementing an interface (§3.2, §3.4) ----
    public class Circle implements Shape {
        private const _radius:Number;                        // const field
        public static const PI:Number = 3.141592653589793;   // static const

        public function Circle(radius:Number) {              // constructor
            _radius = radius;
        }

        public function area():Number {
            return PI * _radius * _radius;
        }

        public function get name():String {                  // getter
            return "circle";
        }

        protected function kind():String {                   // protected member
            return "round";
        }

        public function describe():String {
            return "a " + name + " with area " + area();     // virtual name()/area()
        }
    }

    // ---- final class: inheritance + mandatory override + super + get/set ----
    public final class Ball extends Circle {
        private var _color:String;                           // non-nullable (§4.1)

        public function Ball(radius:Number, color:String) {
            super(radius);                                   // super constructor
            _color = color;
        }

        override public function get name():String {         // 'override' mandatory
            return _color + " ball";
        }

        public function get color():String { return _color; }
        public function set color(c:String):void { _color = c; }   // setter

        override public function describe():String {
            return super.describe() + " (" + kind() + ")";   // super call + protected
        }
    }

    // ---- Reified user generic (§4.2) ----
    public class Box.<T> {
        private var _value:T;
        public function Box(value:T) { _value = value; }
        public function get value():T { return _value; }
    }

    // ---- dynamic class: opt-in expando properties (§3.2) ----
    public dynamic class Bag {
        public var label:String;
        public function Bag(label:String) { this.label = label; }
    }

    // ---- Custom Error subclass in the Error hierarchy (§6) ----
    public class TooBig extends RangeError {
        public function TooBig(message:String) { super(message); }
    }
}

// ============================================================================
// Top-level (default package). The runtime invokes main() (SPECS §7).
// ============================================================================
import demo.*;

function greet(name:String = "world"):String {   // default parameter (§3.4)
    return "hello, " + name;
}

function sum(...nums):int {                       // rest args (§3.7)
    var total:int = 0;
    for each (var n:int in nums) { total += n; }
    return total;
}

function firstOf.<T>(items:Vector.<T>):T {        // generic free function (§4.2)
    return items[0];
}

function checkRange(n:int):void {                 // throws into the hierarchy (§6)
    if (n > 100) {
        throw new TooBig("value " + n + " exceeds 100");
    }
}

function main():int {

    // (1) HELLO WORLD + trace  (§6, Phase 3) ------------------------------
    trace(greet());               // hello, world
    trace(greet("ASR"));          // hello, ASR

    // (2) primitives + coercion  (§3.3) -----------------------------------
    var i:int = -7;
    var u:uint = 42;
    var d:Number = 3.5;
    var b:Boolean = (i < 0);
    trace("int=" + i + " uint=" + u + " num=" + d + " bool=" + b);

    // (3) if / else if / else  (§3.8) -------------------------------------
    var score:int = 83;
    if (score >= 90) {
        trace("grade A");
    } else if (score >= 80) {
        trace("grade B");
    } else if (score >= 70) {
        trace("grade C");
    } else {
        trace("grade F");
    }

    // (4) loops: for, while, labeled break  (§3.8) ------------------------
    var acc:int = 0;
    for (var k:int = 0; k < 5; k++) { acc += k; }
    trace("for sum 0..4 = " + acc);

    var w:int = 3;
    while (w > 0) { w--; }
    trace("while done w=" + w);

    outer:
    for (var x:int = 0; x < 3; x++) {
        for (var y:int = 0; y < 3; y++) {
            if (x + y == 3) break outer;          // labeled break
        }
    }
    trace("labeled break ok");

    // (5) switch with fall-through + default  (§3.8) ----------------------
    var day:int = 6;
    switch (day) {
        case 6:
        case 7:
            trace("weekend");
            break;
        default:
            trace("weekday");
    }

    // (6) Array + Vector.<T>, for-each, rest, generic fn  (§3.10, §4.2–4.3)
    var arr:Array = [10, 20, 30];
    var v:Vector.<int> = new <int>[1, 2, 3, 4];
    var vecTotal:int = 0;
    for each (var val:int in v) { vecTotal += val; }
    trace("vector total = " + vecTotal);
    trace("array[1] = " + arr[1]);
    trace("sum(...) = " + sum(1, 2, 3, 4));
    trace("firstOf = " + firstOf.<int>(v));

    // (7) OOP: interface, inheritance, polymorphism, is/as  (§3.1, §3.4) --
    var shapes:Vector.<Shape> = new <Shape>[ new Circle(2.0), new Ball(1.0, "red") ];
    for each (var s:Shape in shapes) {
        trace(s.name + " area=" + s.area());
        if (s is Ball) {
            var ball:Ball = s as Ball;            // checked downcast
            trace("  it's a ball: " + ball.name);
        }
    }

    // (8) generics + null safety  (§4.1, §4.2) ----------------------------
    var boxed:Box.<String> = new Box.<String>("packed");
    trace("box = " + boxed.value);

    var maybe:String? = null;                     // nullable reference (§4.1)
    if (maybe != null) {
        trace("len=" + maybe.length);             // narrowed to non-null here
    } else {
        trace("maybe is null");
    }
    // Modern conveniences (SPECS §4.6, Phase 6+; shown for reference, not run):
    //   var len:int = maybe?.length ?? 0;   // optional-chaining + nullish-coalescing

    // (9) closures capture lexical scope  (§3.7) --------------------------
    var factor:int = 10;
    var scale:Function = function(n:int):int { return n * factor; };
    trace("closure scale(5) = " + scale(5));

    // (10) method closure keeps 'this' bound  (§3.7) ----------------------
    var target:Circle = new Ball(2.0, "blue");
    var described:Function = target.describe;     // extracted, still bound to target
    trace(described());                           // virtual dispatch + super + protected

    // (11) getter / setter  (§3.4) ----------------------------------------
    var b2:Ball = new Ball(1.0, "red");
    b2.color = "green";
    trace("ball color = " + b2.color);

    // (12) dynamic class expando  (§3.2) ----------------------------------
    var bag:Bag = new Bag("stuff");
    bag.extra = "added at runtime";               // allowed: class is dynamic
    trace("bag " + bag.label + " / " + bag.extra);

    // (13) try / catch / finally + throw + typed catch  (§3.8, §6) --------
    try {
        trace("checking...");
        checkRange(150);                          // throws TooBig
    } catch (e:TooBig) {                          // most-specific clause first
        trace("caught TooBig: " + e.message);
    } catch (e:Error) {
        trace("caught Error: " + e.message);
    } finally {
        trace("cleanup always runs");
    }

    return 0;                                     // process exit code
}

/* ---------------------------------------------------------------------------
EXPECTED STDOUT (golden — SPECS §10). Number formatting = ECMA shortest
round-trip. Exit code 0.

hello, world
hello, ASR
int=-7 uint=42 num=3.5 bool=true
grade B
for sum 0..4 = 10
while done w=0
labeled break ok
weekend
vector total = 10
array[1] = 20
sum(...) = 10
firstOf = 1
circle area=12.566370614359172
red ball area=3.141592653589793
  it's a ball: red ball
box = packed
maybe is null
closure scale(5) = 50
a blue ball with area 12.566370614359172 (round)
ball color = green
bag stuff / added at runtime
checking...
caught TooBig: value 150 exceeds 100
cleanup always runs
--------------------------------------------------------------------------- */
