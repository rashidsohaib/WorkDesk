# WorkDesk Chrome v2.10.3
* Backend rebuilt from source (v2.10.1 feature set kept: scheduled send that survives closing WorkDesk, draft editing, per-alias names, instant opening via background download, contacts, advanced search, Move to Inbox).
* Fix: an inline picture in mail sent from the Gmail web client is no longer also listed as an attachment (backend now treats an image with a Content-ID as inline; the attachment list hides inline images shown in the body).
* Saved App Password is read from the existing vault: no need to re-enter it.
