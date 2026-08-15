# notes made by human, don't take seriously

# todo
- perfect the focus mechanics and focus history.
    - we want to be able to fluidly choose which node to focus on with minimal cognitive effort.
    - we want to track a history of which panels were focused so we can jump back and forth quickly.
    - when fuzzy searching we want to put higher priority on recently visited nodes.
    - the telescope like fuzzy search
- action view: automatically move to locations where things are happening.
    - when the agents are doing stuff. Sit back and relax and watch what they're doing automatically. 
    - we should probably have a centralized event logs of things that are happening. We're already tracking all the files and all the changes we should be able to see all the changes that are happening.
- how to treat agent context
    - we want each agent to in essance have their context be a text file that we can search through.
    - a standard text summarization of an agents history can be fed to any agent harness to have it continue where it left off.
    - we need to figure out how this should work by default. When we open an agent pane in a folder, what happens to the history? With pi there's probably some easy solution here.
- sub-agents and agent-to-agent communication.
    - currently planning for this to be tmux based with utils implemented as a lean cli that supports:
    - roster: shows all active agent tmux sessions open in the graph, what their status is, how long they've been running, etc. hopefully some live updating status of what they're doing.
    - peek: get a larger look at what the agent is currently doing by capturing their tmux pane. We wanna try to make this nice for the agent so it gets useful information.
    - send: sends a message to an agent. Presumably we implement this by just sending keystroke into the tmux pane. Although the send tool shoudl take care of some of the rought bits for us. The send tool should also include template stuff like which agent is sending the message and other potentially useful metadata.
    - maybe this can also log all the messages the agents are sending to each other somewhere so we can get an overview of it.
- images/pdf/audio and other files.
- make LLM wiki setup: https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f 
    - how to do the entire workflow? 
    - ingest/query/lint
- figure out how to use yaml frontmatter correctly.
    - each file should have some mandatory metadata like date created, date modified, immutable id.
    - quick description and purpose of this file.
    - tags
- fuzzy search across the entire graph including contents of the text files.
- make it so I can drag and drop files and paste shit into the gui.

# in progress

# done
- when I start a new terminal/agent it should be focused by default. also I wanna start adding more keybindngs. For example when I have a folder focused and I press 'a' then the default agent pane should open in that folder. When I press t a terminal pane should open. (remove the existing keybinding that says t shoudl jump between panes. we don't need that). when I press e it should open the node in my default editor, whether it is a text file or a folder.
- have a menu for settings.
    - light mode vs. dark mode
    - default agent and then when I click the launch agent button without selecting a specific one it launches the default one. Pi should be the default for now.
- there should be a quick option to open a text file in a terminal with my default terminal editor. and then that terminal gets linked to that specific text node.
- when I'm the rangelike navigation and the camera jumps around, please make it so that it doesn't instantly snap, but instead does a quick movement. This will make it easdier to see where we're jumping from and where we're jumping to. And also make it maintain the same zoom the camera already has, but the camera just refocuses location.
- make graph edges directed and make folder edges more noticable. The folder structure should be clear glancing at the graph.
- make it so the focused pane/terminal is always on top of the non-focused one. Currently other terminals appear on top of the focues one.
- make it so node icons render on top of edges. Currently the terminal edges going from a folder to a terminal with the dotted eges goes on top of the folder icon which is wrong.
- external web links
- Make the ranger style navigator and file viewer in the side bar prettier. Currently we don't have good icons for files or folders, and also we need to render the markdown files well and take into account obsidian style references to stuff like [[ref]] and also for external links and images. 
- when I click to expand a terminal pane / agent pane it should expand so the the preview is in the center of the entire terminal. Currently the preview is like the top left part, but this is unintuitive because I place the preview at the place where I want the terminal to expand not wehre I want the top-leftt to expand from. DOes that make sense? Do you understand what i mean? 
- when hovering over a node I should be able to see metadata like, when it was created, when it was last edited, how long it is, how many ingoing and outgoing references it has and how many of those are to files in the vault and which ones are external links. I should be able to see size of the file too. Same when hovering over a folder. I should be able to see various stats like  number of files in the folder, 1 layer deep and all layers deep. How many outgoing links it has in total etc.
