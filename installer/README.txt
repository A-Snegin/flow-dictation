Flow
Local dictation for Windows. Hold a key, talk, let go, the text is there.


GETTING STARTED

Hold Right Ctrl, speak, and let go. The text appears wherever your cursor is,
in any application.

Flow sits in the notification area, next to the clock. Right-click it for
settings, or to pause it, or to quit.

The first time you hold the key, Windows will ask whether Flow can use your
microphone. Say yes; without it there is nothing to transcribe.


PRIVACY

Everything runs on your computer. Your voice is never sent anywhere and no
audio is ever written to disk. There is no account, no sign-in and no network
connection involved in dictating.

Flow stores three things, all on this machine:

  %APPDATA%\Flow\settings.toml       your settings and dictionary
  %LOCALAPPDATA%\Flow\traces.jsonl   timings only, never what you said
  %LOCALAPPDATA%\Flow\models         the speech model

Pasting passes the text through the Windows clipboard for a moment. Flow marks
it so Clipboard History does not keep a copy, and puts your previous clipboard
contents back afterwards. If you would rather it never touched the clipboard,
set insertion to "Type" in the settings.


YOUR OWN WORDS

Names, jargon and company names are what a recogniser gets wrong most often, so
Flow lets you tell it yours. Open the settings and add them to the dictionary,
one per line:

  Lift-Off Consulting
  DDDM

Those are biased while it listens, so it is more likely to get them right in
the first place, and corrected afterwards if it did not.

If a term is heard one way and should be written another, use both sides:

  lift off = Lift-Off


SPEAKING PUNCTUATION

Say "comma", "full stop", "question mark", "new line" or "new paragraph" and
they become the real thing. Sentences are capitalised for you.

In a terminal, line breaks are turned into spaces, so dictating can never press
Enter and run a command by accident.


IF SOMETHING IS WRONG

  Nothing happens when I hold the key
    Check the notification-area icon is there and not paused. Another
    application may have taken the same key; try a different one in settings.

  The first word is missing
    Start speaking a moment after pressing, rather than at the same time.

  It types into the wrong place
    Flow inserts wherever the cursor is. Click into the box you want first.

  It is slower than you would like
    Settings, Model, choose Fast. Roughly twice as quick, a little less
    accurate on difficult audio.


REMOVING IT

Settings, Apps, Flow, Uninstall. It will ask whether to keep the speech model
and your dictionary, in case you are reinstalling.


Built by Lift-Off Consulting.
Speech recognition by Moonshine (moonshine.ai), MIT licensed.
