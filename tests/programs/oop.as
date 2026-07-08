package demo {
    public interface Shape {
        function area():Number;
        function get name():String;
    }

    public class Circle implements Shape {
        private var _radius:Number;
        public static const PI:Number = 3.141592653589793;
        public static var count:int = 0;

        public function Circle(radius:Number) {
            _radius = radius;
            count++;
        }

        public function get radius():Number { return _radius; }
        public function set radius(r:Number):void { _radius = r; }

        public function area():Number {
            return PI * _radius * _radius;
        }

        public function get name():String {
            return "circle";
        }

        public function describe():String {
            return name + "(r=" + _radius + ")";
        }

        public function toString():String {
            return "<" + describe() + ">";
        }
    }

    public final class Ball extends Circle {
        private var _color:String = "plain";

        public function Ball(radius:Number, color:String) {
            super(radius);
            _color = color;
        }

        override public function get name():String {
            return _color + " ball";
        }

        public function darken():String {
            return "dark " + super.describe();
        }
    }

    public class Unit extends Circle {
        public function Unit() {
            super(1);
        }
    }
}

var shapes0:Shape = new demo.Ball(2, "red");
var c:Circle = new Circle(1.5);
c.radius = c.radius * 2;
trace(shapes0.name, shapes0.area() > 12, shapes0 is Circle, shapes0 is Shape);
trace(c.name, c.radius, c.describe());
trace(new Unit().area() == Circle.PI, Circle.count);
var b:Ball = shapes0 as Ball;
trace(b.darken());
trace(b);
var miss:Ball = c as Ball;
trace(miss == null, Object(b) === b);
