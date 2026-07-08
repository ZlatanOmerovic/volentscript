class Base {
    public function greet():String { return "hi"; }
    public final function id():int { return 1; }
    private var secret:int = 1;
}
interface Talker {
    function talk():String;
}
class Bad extends Base implements Talker {
    public function greet():String { return "yo"; }         // E0305 missing override
    override public function id():int { return 2; }         // E0305 final override
    override public function nothing():void {}              // E0305 overrides nothing
}                                                            // + missing talk()
class Cycle extends Cycle {}                                 // E0305 self-inherit
final class Sealed {}
class Child extends Sealed {}                                // E0305 extends final
var b:Base = new Base();
trace(b.secret);                                             // E0307 private
b.id(1);                                                     // E0303 arity
var t:Talker = b;                                            // E0302 not implementing
var x:int = new Base();                                      // E0302 class -> int
