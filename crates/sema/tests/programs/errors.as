// P2 milestone corpus (negative): every diagnostic must carry the right
// span and a stable E03xx code.

var wrong:int = "twelve";          // E0302 String -> int
var zilch:Number = null;           // E0302 null -> machine type
var mystery:Thing = 1;             // E0301 unknown type
trace(missing);                    // E0301 unresolved name
var five:int = 5;
five();                            // E0306 not callable
parseInt("1", 10, 16);             // E0303 too many args
const FROZEN:int = 1;
FROZEN = 2;                        // E0304 assign to const
var twice:int = 1;
var twice:String = "again";        // E0305 conflicting redeclaration
"hello".missing;                   // E0307 unknown property on sealed String
"hello".length = 9;                // E0307 write to read-only member

function needsInt(n:int):void {}
needsInt("nope");                  // E0302 in argument position

function broken(flag:Boolean):String {
    if (flag)
        return "yes";
}                                  // E0308 falls off the end

function voidness():void {}
var v:int = voidness();            // E0309 void as value

break;                             // E0310 outside loop
missing2: for (;;) { break; }
continue missing2;                 // E0310 label not in scope (loop ended)

var isIt:Boolean = five is missing3;   // E0311 not a type
var old:Boolean = five instanceof int; // warning: deprecated
