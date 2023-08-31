pragma circom 2.0.0;

// a * b * b == 1001
// a, b, and c must all be different, and all > 1
// solution is: 7, 11, 13, in any permutation
template scheherazade() {
    signal input a;
    signal input b;
    signal input c;
    signal ab;
    signal output d;
    ab <== a*b;
    d <== ab * c;
 }

 component main = scheherazade();