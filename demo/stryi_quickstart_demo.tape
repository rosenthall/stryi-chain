Output demo/stryi-quickstart.gif

Set Shell bash
Set Theme "Builtin Tango Dark"
Set FontFamily "Menlo"
Set Width 1760
Set Height 960
Set Padding 16
Set Margin 24
Set MarginFill "#0ABAB5"
Set BorderRadius 18
Set WindowBar Colorful
Set FontSize 17
Set LineHeight 1.0
Set Framerate 45
Set PlaybackSpeed 1.0
Set TypingSpeed 60ms
Set CursorBlink false

Env HOME "/tmp/stryi-demo/home"
Env CLICOLOR_FORCE "1"
Env COLORTERM "truecolor"

Hide
Type `export PS1="$ "`
Enter
Sleep 200ms
Type "unset NO_COLOR"
Enter
Sleep 200ms
Type "unset RUST_LOG"
Enter
Sleep 200ms
Type "export TERM=xterm-256color"
Enter
Sleep 200ms
Type "rm -rf /tmp/stryi-demo && mkdir -p $HOME"
Enter
Sleep 200ms
Type "./target/release/stryi-wallet import --key 3a7dc55eecb56b8f36874f6920e56a4c4c103ea82b5a168d073d2b49394f3499 --label alice >/dev/null"
Enter
Sleep 200ms
Type "./target/release/stryi-wallet import --key 9a95bf6f928f55be8e757917cdc20a3a12d4e2fcfd5b01243d6db6db7005b019 --label bob >/dev/null"
Enter
Sleep 200ms
Type "tmux kill-session -t stryi-demo 2>/dev/null || true"
Enter
Sleep 200ms
Type `tmux -f /dev/null new-session -d -s stryi-demo "env PS1='$ ' bash --noprofile --norc -i"`
Enter
Sleep 200ms
Type "tmux set-option -t stryi-demo status off"
Enter
Sleep 200ms
Type "tmux set-option -t stryi-demo pane-border-status top"
Enter
Sleep 200ms
Type `tmux split-window -v -p 38 -t stryi-demo:0 "env PS1='$ ' bash --noprofile --norc -i"`
Enter
Sleep 200ms
Type "tmux select-pane -t stryi-demo:0.0 -T node"
Enter
Sleep 200ms
Type "tmux select-pane -t stryi-demo:0.1 -T wallet"
Enter
Sleep 200ms
Type "tmux select-pane -t stryi-demo:0.0"
Enter
Sleep 200ms
Type "tmux send-keys -t stryi-demo:0.0 'clear' C-m"
Enter
Sleep 200ms
Type "tmux send-keys -t stryi-demo:0.1 'clear' C-m"
Enter
Sleep 200ms
Type "tmux attach -t stryi-demo"
Enter
Sleep 500ms
Show
Sleep 900ms

Type "./target/release/stryi-devkit chaingen --config-path demo/chaingen.toml"
Enter
Wait+Screen /Successfully read and deserialized the config\./
Sleep 1800ms

Hide
Type `export RUST_LOG='warn,stryi_node::node=info,stryi_node::miner=info,stryi_node::http=info'`
Enter
Sleep 250ms
Show
Sleep 350ms

Type "./target/release/stryi-node --config-path demo/node.toml --genesis-config-path demo/genesis.json"
Enter
Wait+Screen /HTTP API listening on 127\.0\.0\.1:5556/
Sleep 3500ms

Hide
Ctrl+B
Type "o"
Sleep 300ms
Show
Sleep 500ms

Type "./target/release/stryi-wallet nodestate"
Enter
Wait+Screen /Node State/
Sleep 2200ms

Type "./target/release/stryi-wallet send --from @4c8c01f08adc9162ff3d137389399634a375d6d6 --to @3972d0819c496cadff43cc37f99a9322745aa397 --amount 25000"
Enter
Wait+Screen /Accepted by node/
Wait+Screen /Mined block #21 by/
Sleep 2200ms

Type "./target/release/stryi-wallet nodestate"
Enter
Sleep 2400ms

Hide
Ctrl+B
Type "d"
Sleep 300ms
Type "tmux kill-session -t stryi-demo"
Enter
Sleep 400ms
