# Function Arity

## Identity

A function identity is its qualified name plus its arity. Arity counts every
parameter, including `self`.

Separate declarations can share a name only when their arities differ. Two
declarations with the same qualified name collide when they share an arity,
even if the parameter types differ. A declaration cannot define defaults whose
accepted arity range overlaps another declaration with the same qualified name.

Koja keeps `self` explicit. Function boundaries show state flow even when a
function body uses imperative code.

## Default parameters

Default parameters must follow all required parameters. A declaration accepts
each arity from its required parameter count through its total parameter count.
Each default is independent and cannot refer to `self` or another parameter.

Koja evaluates each omitted default for every call. Protocol declarations own
their defaults, so implementations do not replace them.

## Named function values

Every named function value uses an explicit reference with an arity:

```koja
&name/2
&Package.name/1
&Type.function/3
&Point.translate/3
```

`&Point.translate/3` has type `fn (Point, Int, Int) -> Point`. Arity counts
`self`.

The arity makes the function identity explicit. A bare function name is not a
function value. Every named function value uses mandatory `&name/arity`,
including a single-arity function.

Generic named function references are not supported. Use a closure when a
function needs inferred type arguments or adaptation.

Default parameters prove the model. `JSON.encode(value, JSON.EncodeOptions{})`
accepts one or two arguments because `EncodeOptions` declares
`pretty?: Bool = false`.

Bound references and named arguments are outside this design.
