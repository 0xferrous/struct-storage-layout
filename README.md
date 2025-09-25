# struct-storage-layout

Give output equivalent to `forge inspect <Contract> storage` but for solidity structs.
The command itself doesn't work when you are using something like

```solidity
contract Hello {
    struct Foo {
        uint a;
        bytes4 b;
        bool c;
        int88 d;
        uint e;
    }

    bytes32 constant FOO_SLOT = keccak256("Hello.storage.foo");

    function _getStorage() public view returns (Foo storage _foo) {
        bytes32 slot = FOO_SLOT;
        assembly {
            _foo.slot := slot;
        }
    }
}
```

This repo is an attempt to bridge that gap. It is custom and very primitive parsing. Eventually
would like to integrate solar to parse the source into an AST and then just walk on the AST.
Also, eventually would like a way to read a struct from a given contract and slot in alloy itself
by computing the necessary slots for struct values on the fly and then decoding them into
specific struct fields.
