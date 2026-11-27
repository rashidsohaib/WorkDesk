# WorkDesk Chrome v2.10.4
* Slow opening of some messages fixed. A conversation lists its messages by their All Mail copy, while the background download had stored the Inbox / Sent copy: the same message looked "not downloaded" and was fetched again from Gmail. Messages are now matched by their Gmail message id, so any copy opens from memory.
* Messages that really are not in memory are fetched on a ready connection kept open for a few minutes, instead of a fresh Gmail login (several seconds) per message.
* Opening a conversation starts downloading its other messages at once (newest first); a click never downloads a message that is already being downloaded.
