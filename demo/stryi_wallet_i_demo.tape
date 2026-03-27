Output demo/stryi-demo.gif

Set Shell bash
Set Theme "Builtin Tango Dark"
Set FontFamily "Menlo"
Set Width 1360
Set Height 980
Set Padding 24
Set Margin 32
Set MarginFill "#0ABAB5"
Set BorderRadius 18
Set WindowBar Colorful
Set FontSize 18
Set LineHeight 1.1
Set Framerate 45
Set TypingSpeed 110ms
Set CursorBlink false

Env HOME "/tmp/stryi-demo/home"
Env CLICOLOR_FORCE "1"
Env COLORTERM "truecolor"

# - setup stage -
# it is not visible in the gif
# Generate the chain, preload the wallet, and boot the local node.
Hide
Type "cd target/release"
Enter
Sleep 300ms
Type `export PS1="$ "`
Enter
Sleep 300ms
Type "rm -rf /tmp/stryi-demo"
Enter
Sleep 400ms
Type "mkdir -p $HOME"
Enter
Sleep 300ms

Type "./stryi-devkit chaingen --config-path ../../demo/chaingen.toml"
Enter
Wait+Screen /Successfully read and deserialized the config\./
Sleep 300ms

# import keys
Type "./stryi-wallet import --key 3a7dc55eecb56b8f36874f6920e56a4c4c103ea82b5a168d073d2b49394f3499 --label alice"
Enter
Sleep 700ms
Type "./stryi-wallet import --key 9a95bf6f928f55be8e757917cdc20a3a12d4e2fcfd5b01243d6db6db7005b019 --label bob"
Enter
Sleep 700ms

Type "./stryi_node --config-path ../../demo/node.toml --genesis-config-path ../../demo/genesis.json > /tmp/stryi-demo/node.log 2>&1 &"
Enter
Sleep 1200ms

Type "until ./stryi-wallet nodestate >/dev/null 2>&1; do sleep 0.2; done"
Enter
Sleep 300ms

Type "clear"
Enter
Sleep 250ms
Show
Sleep 900ms

# Visible demo start
Type "./stryi-wallet -i"
Enter
Wait+Screen /wallet\(2\)>/
Sleep 1200ms


Type "help"
Enter
Sleep 3200ms

Type "clear"
Enter
Sleep 700ms

Type "list"
Enter
Wait+Screen /bob/
Sleep 2200ms

Type "balance"
Enter
Wait+Screen /500000/
Sleep 2600ms

Type "send"
Enter
Wait+Screen /From \(# or address\):/
Sleep 1500ms

Type "1"
Enter
Wait+Screen /To \(# or address\):/
Sleep 900ms

Type "2"
Enter
Wait+Screen /Amount:/
Sleep 1500ms

Type "25000"
Enter
Wait+Screen /Transfer Preview/
Sleep 1800ms

Type "y"
Sleep 2050ms

Enter
Sleep 7000ms

Type "exit"
Enter
Sleep 600ms

Hide
Type "pkill -f stryi_node || true"
Enter
Sleep 300ms
