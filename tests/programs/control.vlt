var total:int = 0;
outer: for (var i:int = 0; i < 5; i++) {
    for (var j:int = 0; j < 5; j++) {
        if (j > i)
            continue outer;
        if (i == 4)
            break outer;
        total += 1;
    }
}
trace(total);

var k:int = 3;
while (k > 0)
    k--;
do {
    k++;
} while (k < 2);
trace(k);

switch (k) {
    case 1:
        trace("one");
        break;
    case 2:
        trace("two");
    default:
        trace("fell through");
}

var grade:String = k >= 2 ? "pass" : "fail";
trace(grade);
