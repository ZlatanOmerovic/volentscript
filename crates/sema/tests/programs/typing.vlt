// P2 milestone corpus (positive): core typing and coercion insertion.

var count:int = 0;
var total:Number = count;          // int -> Number coercion
var ratio:Number = count / 3;      // division is Number even for ints
var bits:int = count & 0xFF;
var high:uint = count >>> 1;
var label:String = "n = " + count; // concat coerces the int side
var anything:* = label;            // T -> *
var narrowed:int = anything;       // * -> int (runtime-checked)
var flag:Boolean = anything is int;
var maybe:String? = anything as String;
const LIMIT:int = 100;

function scale(value:Number, factor:Number = 2):Number {
    return value * factor;
}

function shout(message:String):void {
    trace(message.toUpperCase(), message.length);
}

function pick(useFirst:Boolean, a:int, b:Number):Number {
    return useFirst ? a : b;       // branch merge: int/Number -> Number
}

function classify(n:int):String {
    if (n < 0) {
        return "negative";
    } else if (n == 0) {
        return "zero";
    }
    switch (n % 2) {
        case 0:
            return "even";
        default:
            return "odd";
    }
}

function sum(...values):Number {
    var acc:Number = 0;
    for each (var v:Number in values) {
        acc += v;
    }
    return acc;
}

count &&= LIMIT;
count ||= parseInt("42", 10);      // Number -> int on assignment
total = scale(count);              // defaulted argument
shout(classify(count));
trace(sum(1, 2.5, count), pick(true, 1, 2.5).toFixed(2));

outer: while (count < LIMIT) {
    count++;
    if (count % 7 == 0)
        continue outer;
    if (count > 50)
        break outer;
}

try {
    throw "level " + count;
} catch (e:String) {
    trace("caught:", e);
} catch (e) {
    trace(typeof e);
} finally {
    count = 0;
}
