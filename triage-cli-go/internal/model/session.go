package model

// InvestigationSession is the in-flight state of a single triage run.
// It accumulates evidence and the resolved timeline before an Assessment
// and Report are produced.
type InvestigationSession struct {
	Ticket     Ticket
	Evidence   []Evidence
	Timeline   []TimelineEvent
	Assessment *Assessment
	Report     *TriageReport
}
