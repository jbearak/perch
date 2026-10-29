# Perch

Persistent terminal sessions for VS Code and its open-source forks.

Perch keeps R, Stata and shell sessions alive after you close the editor window or lose an SSH connection. It is a companion to [Raven](https://github.com/jbearak/raven) (R) and Sight (Stata), and it works on its own for plain shells.

Status: design agreed, implementation starting.

- [Brief](docs/brief.md): the requirements Perch started from.
- [Design](docs/design.md): how Perch works and why, compared with tmux, dtach, abduco, mosh, shpool and zmx.
- [Implementation plan](docs/plan.md): the milestones and how each is proven.
- [Manual verification checklist](docs/verification.md): checks that cannot run in CI.

Licensed under the GNU General Public License v3. See [LICENSE](LICENSE).
